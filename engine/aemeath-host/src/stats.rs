//! 每秒一次的性能统计：引擎进程的 CPU 占用、调度线程的峰值负载和各节点的 ASRC 缓冲占用。
//! ASRC 占用比超出标题栏的警告范围时记日志，标题栏上一闪而过的异常事后也能在日志里查到。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use aemeath_engine::Engine;
use aemeath_engine::devices::BufferSource;
use aemeath_protocol::ids::NodeId;
use aemeath_protocol::message::{ASRC_WARN_RANGE, BufferStat, EngineToApp};
use aemeath_protocol::topology::RouteTopology;
use aemeath_sys::process::current_process_cpu_hns;

use crate::status::{node_for_program, nodes_for};

/// 统计的发送间隔
pub const STATS_PERIOD: Duration = Duration::from_secs(1);
const HNS_PER_SECOND: f64 = 10_000_000.0;
/// 回到范围内持续这么久才算一次越界结束。期间反复进出合并成一次，在阈值附近来回跳时不会每秒记一条
const ASRC_SETTLE: Duration = Duration::from_secs(5);

/// 记下上一次的 CPU 时间，算出两次之间的占用。
pub struct StatsSampler {
    last_cpu_hns: Option<u64>,
    last_wall: Instant,
    processors: usize,
    asrc: AsrcWatch,
}

impl StatsSampler {
    pub fn new() -> Self {
        Self {
            last_cpu_hns: current_process_cpu_hns(),
            last_wall: Instant::now(),
            processors: std::thread::available_parallelism().map_or(1, |count| count.get()),
            asrc: AsrcWatch::default(),
        }
    }

    /// 取一次统计；ASRC 缓冲按 `topology` 对应到节点。
    pub fn sample(&mut self, engine: &Engine, topology: Option<&RouteTopology>) -> EngineToApp {
        let cpu_hns = current_process_cpu_hns();
        let now = Instant::now();
        let cpu_percent = match (self.last_cpu_hns, cpu_hns) {
            (Some(last), Some(current)) => cpu_percent(
                current.saturating_sub(last),
                now.duration_since(self.last_wall),
                self.processors,
            ),
            _ => 0.0,
        };
        self.last_cpu_hns = cpu_hns;
        self.last_wall = now;

        let mut buffers = Vec::new();
        for (source, (level_ms, target_ms)) in engine.buffer_levels() {
            let stat = |node| BufferStat {
                node,
                level_ms,
                target_ms,
            };
            match &source {
                BufferSource::Device(endpoint, direction) => {
                    buffers.extend(nodes_for(topology, endpoint, *direction).map(stat));
                }
                BufferSource::Program(program) => buffers.extend(node_for_program(topology, program).map(stat)),
            }
        }
        for event in self.asrc.observe(now, &buffers) {
            match event {
                AsrcEvent::Left {
                    node,
                    percent,
                    level_ms,
                    target_ms,
                } => tracing::warn!(
                    node = node.0,
                    percent = format!("{percent:.0}"),
                    level_ms = format!("{level_ms:.2}"),
                    target_ms = format!("{target_ms:.2}"),
                    "ASRC buffer level left the normal range"
                ),
                AsrcEvent::Returned {
                    node,
                    seconds,
                    extreme_percent,
                } => tracing::info!(
                    node = node.0,
                    seconds,
                    extreme_percent = format!("{extreme_percent:.0}"),
                    "ASRC buffer level back in the normal range"
                ),
            }
        }
        EngineToApp::EngineStats {
            cpu_percent,
            realtime_peak_percent: engine.take_realtime_peak_percent(),
            buffers,
        }
    }
}

/// 一个节点的 ASRC 占用比超出警告范围的经过。
struct Excursion {
    started: Instant,
    last_outside: Instant,
    /// 期间偏离 100% 最远的占用比
    extreme_percent: f32,
}

#[derive(Debug, PartialEq)]
enum AsrcEvent {
    Left {
        node: NodeId,
        percent: f32,
        level_ms: f32,
        target_ms: f32,
    },
    Returned {
        node: NodeId,
        /// 第一次到最后一次超出范围的每秒统计覆盖的秒数
        seconds: u64,
        extreme_percent: f32,
    },
}

/// 跟踪各节点的 ASRC 占用比，超出范围和回到范围时各报一次。
#[derive(Default)]
struct AsrcWatch {
    excursions: HashMap<NodeId, Excursion>,
}

impl AsrcWatch {
    fn observe(&mut self, now: Instant, buffers: &[BufferStat]) -> Vec<AsrcEvent> {
        let mut events = Vec::new();
        for buffer in buffers {
            let Some(percent) = buffer.percent() else {
                continue;
            };
            let outside = !ASRC_WARN_RANGE.contains(&percent);
            let node = buffer.node;
            match self.excursions.get_mut(&node) {
                Some(excursion) if outside => {
                    excursion.last_outside = now;
                    if (percent - 100.0).abs() > (excursion.extreme_percent - 100.0).abs() {
                        excursion.extreme_percent = percent;
                    }
                }
                Some(excursion) if now.duration_since(excursion.last_outside) >= ASRC_SETTLE => {
                    // 每条统计是一秒的平均，所以一次越界至少算一秒
                    let covered = excursion.last_outside.duration_since(excursion.started) + STATS_PERIOD;
                    events.push(AsrcEvent::Returned {
                        node,
                        seconds: covered.as_secs_f32().round() as u64,
                        extreme_percent: excursion.extreme_percent,
                    });
                    self.excursions.remove(&node);
                }
                Some(_) => {}
                None if outside => {
                    self.excursions.insert(
                        node,
                        Excursion {
                            started: now,
                            last_outside: now,
                            extreme_percent: percent,
                        },
                    );
                    events.push(AsrcEvent::Left {
                        node,
                        percent,
                        level_ms: buffer.level_ms,
                        target_ms: buffer.target_ms,
                    });
                }
                None => {}
            }
        }
        // 节点删除或设备离线后不再上报，越界的记录直接丢掉
        self.excursions
            .retain(|node, _| buffers.iter().any(|buffer| buffer.node == *node));
        events
    }
}

/// 一段时间内的 CPU 时间换算成占全部逻辑处理器的百分比（同任务管理器）。
fn cpu_percent(cpu_hns: u64, wall: Duration, processors: usize) -> f32 {
    let available = wall.as_secs_f64() * HNS_PER_SECOND * processors.max(1) as f64;
    if available <= 0.0 {
        return 0.0;
    }
    (cpu_hns as f64 / available * 100.0).min(100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_is_spread_over_all_processors() {
        // 一秒里用了半个核的时间，8 个逻辑处理器
        assert!((cpu_percent(5_000_000, Duration::from_secs(1), 8) - 6.25).abs() < 1e-4);
        assert!((cpu_percent(20_000_000, Duration::from_secs(2), 1) - 100.0).abs() < 1e-4);
        assert_eq!(cpu_percent(1_000, Duration::ZERO, 8), 0.0);
    }

    fn buffer(node: u32, percent: f32) -> BufferStat {
        BufferStat {
            node: NodeId(node),
            level_ms: percent / 100.0 * 20.0,
            target_ms: 20.0,
        }
    }

    #[test]
    fn asrc_excursions_are_reported_once_and_merged_until_settled() {
        let mut watch = AsrcWatch::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);

        assert!(watch.observe(at(0), &[buffer(1, 100.0), buffer(2, 100.0)]).is_empty());
        let events = watch.observe(at(1), &[buffer(1, 200.0), buffer(2, 100.0)]);
        assert!(matches!(events.as_slice(), [AsrcEvent::Left { node: NodeId(1), .. }]));
        // 在阈值附近来回跳：回到范围内不到 5 秒又出去，不再报
        assert!(watch.observe(at(2), &[buffer(1, 120.0), buffer(2, 100.0)]).is_empty());
        assert!(watch.observe(at(3), &[buffer(1, 260.0), buffer(2, 100.0)]).is_empty());
        for second in 4..8 {
            assert!(
                watch
                    .observe(at(second), &[buffer(1, 100.0), buffer(2, 100.0)])
                    .is_empty()
            );
        }
        let events = watch.observe(at(8), &[buffer(1, 100.0), buffer(2, 100.0)]);
        assert_eq!(
            events,
            [AsrcEvent::Returned {
                node: NodeId(1),
                seconds: 3,
                extreme_percent: 260.0,
            }]
        );
        assert!(watch.observe(at(9), &[buffer(1, 100.0), buffer(2, 100.0)]).is_empty());
    }

    #[test]
    fn asrc_low_levels_and_vanished_nodes() {
        let mut watch = AsrcWatch::default();
        let start = Instant::now();
        let events = watch.observe(start, &[buffer(1, 40.0)]);
        assert!(matches!(events.as_slice(), [AsrcEvent::Left { node: NodeId(1), .. }]));
        // 节点消失后记录丢掉，重新出现时当作新的越界
        assert!(watch.observe(start + Duration::from_secs(1), &[]).is_empty());
        let events = watch.observe(start + Duration::from_secs(2), &[buffer(1, 40.0)]);
        assert!(matches!(events.as_slice(), [AsrcEvent::Left { node: NodeId(1), .. }]));
        // 没有目标占用的缓冲不参与判断
        let empty = BufferStat {
            node: NodeId(2),
            level_ms: 5.0,
            target_ms: 0.0,
        };
        assert!(
            watch
                .observe(start + Duration::from_secs(3), &[buffer(1, 40.0), empty])
                .is_empty()
        );
    }
}
