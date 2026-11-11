//! 槽位重采样器的构建和释放放在辅助线程：调度线程是实时线程，构建 Sinc 滤波器表要几毫秒（实测最长约 4.8 ms），
//! 会让所有槽位和混音一起迟到。调度线程只发请求、在循环里收结果，都不阻塞。

use std::io;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use aemeath_usbip::server::ConnectionId;

use crate::slot::{ResamplerSpec, ResamplerWork, SlotResampler};

enum Job {
    Build(ConnectionId, ResamplerSpec),
    /// 换下来的重采样器，大块内存也不在调度线程上释放
    Retire(SlotResampler),
}

/// 调度线程持有。析构时请求通道关闭，辅助线程随之退出。
pub struct ResamplerWorker {
    jobs: Sender<Job>,
    built: Receiver<(ConnectionId, SlotResampler)>,
}

impl ResamplerWorker {
    /// 启动辅助线程。
    pub fn start() -> io::Result<Self> {
        let (jobs, requests) = channel();
        let (finished, built) = channel();
        thread::Builder::new()
            .name("aemeath-resampler-builder".to_owned())
            .spawn(move || run(&requests, &finished))?;
        Ok(Self { jobs, built })
    }

    /// 交出槽位流要求的工作：先释放换下来的旧重采样器，再请求构建新的。结果由 [`Self::try_receive`] 取回。
    pub fn submit(&self, connection: ConnectionId, work: ResamplerWork) {
        if let Some(retired) = work.retired {
            self.retire(retired);
        }
        if let Some(spec) = work.spec {
            let _ = self.jobs.send(Job::Build(connection, spec));
        }
    }

    /// 把不再使用的重采样器交给辅助线程释放。
    pub fn retire(&self, resampler: SlotResampler) {
        let _ = self.jobs.send(Job::Retire(resampler));
    }

    /// 取一个构建好的重采样器，没有时立刻返回 None。
    pub fn try_receive(&self) -> Option<(ConnectionId, SlotResampler)> {
        self.built.try_recv().ok()
    }
}

/// 辅助线程的主循环：依次构建或释放重采样器，直到请求通道关闭。
fn run(requests: &Receiver<Job>, finished: &Sender<(ConnectionId, SlotResampler)>) {
    while let Ok(job) = requests.recv() {
        match job {
            Job::Build(connection, spec) => {
                if let Some(built) = spec.build() {
                    let _ = finished.send((connection, built));
                }
            }
            Job::Retire(resampler) => drop(resampler),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aemeath_protocol::ids::{SlotDirection, SlotId, SlotNumber};
    use aemeath_protocol::topology::Resampler;
    use aemeath_protocol::units::SampleRate;
    use std::time::{Duration, Instant};

    #[test]
    fn builds_off_the_calling_thread_and_hands_the_result_back() {
        let worker = ResamplerWorker::start().unwrap();
        let spec = ResamplerSpec {
            slot: SlotId {
                direction: SlotDirection::In,
                number: SlotNumber::new(1).unwrap(),
            },
            generation: 7,
            quality: Resampler::default(),
            input_rate: SampleRate::new(44_100).unwrap(),
            output_rate: SampleRate::HZ_48000,
        };
        let connection = ConnectionId(3);
        worker.submit(
            connection,
            ResamplerWork {
                retired: None,
                spec: Some(spec),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let received = loop {
            if let Some(received) = worker.try_receive() {
                break received;
            }
            assert!(Instant::now() < deadline, "nothing was built");
            thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(received.0, connection);
        assert_eq!(received.1.generation(), 7);
    }
}
