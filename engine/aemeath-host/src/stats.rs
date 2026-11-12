//! 每秒一次的性能统计：引擎进程的 CPU 占用、调度线程的峰值负载和各节点的 ASRC 缓冲占用。

use std::time::{Duration, Instant};

use aemeath_engine::Engine;
use aemeath_engine::devices::BufferSource;
use aemeath_protocol::message::{BufferStat, EngineToApp};
use aemeath_protocol::topology::RouteTopology;
use aemeath_sys::process::current_process_cpu_hns;

use crate::status::{node_for_program, nodes_for};

/// 统计的发送间隔
pub const STATS_PERIOD: Duration = Duration::from_secs(1);
const HNS_PER_SECOND: f64 = 10_000_000.0;

/// 记下上一次的 CPU 时间，算出两次之间的占用。
pub struct StatsSampler {
    last_cpu_hns: Option<u64>,
    last_wall: Instant,
    processors: usize,
}

impl StatsSampler {
    pub fn new() -> Self {
        Self {
            last_cpu_hns: current_process_cpu_hns(),
            last_wall: Instant::now(),
            processors: std::thread::available_parallelism().map_or(1, |count| count.get()),
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
        EngineToApp::EngineStats {
            cpu_percent,
            realtime_peak_percent: engine.take_realtime_peak_percent(),
            buffers,
        }
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
}
