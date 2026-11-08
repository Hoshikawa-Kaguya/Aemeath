//! 门限（GLOSSARY：门限）：信号峰值低于阈值时关闭这一路，高于阈值时重新打开。
//!
//! 用户只调阈值；启动、保持、释放时间是固定的内部常量，左右声道共用一个检测（一起开关）。
//! 开合都是线性的增益过渡，不会跳变。实时线程中使用：不分配内存，每帧常数时间。

use crate::gain::db_to_linear;

/// 打开所用的时间：足够短，不吃掉语音的起音
const ATTACK_SECONDS: f64 = 0.001;
/// 信号低于阈值后先保持打开这么久，避免在音节之间、低频的过零点处反复开合
const HOLD_SECONDS: f64 = 0.100;
/// 关闭所用的时间：足够长，尾音自然消失
const RELEASE_SECONDS: f64 = 0.150;
const OPEN: f32 = 1.0;
const CLOSED: f32 = 0.0;

/// 一个节点的门限状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gate {
    /// 线性阈值；None 表示门限关闭（一直打开）
    threshold: Option<f32>,
    gain: f32,
    attack_step: f32,
    release_step: f32,
    hold_frames: u32,
    hold_remaining: u32,
}

impl Gate {
    /// `sample_rate` 是引擎采样率（Hz）。新建的门限处于打开状态，避免拓扑生效时先把声音切掉。
    pub fn new(sample_rate: u32, threshold_db: Option<f32>) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let hold_frames = (rate * HOLD_SECONDS).round() as u32;
        Self {
            threshold: threshold_db.map(db_to_linear),
            gain: OPEN,
            attack_step: (1.0 / (rate * ATTACK_SECONDS).max(1.0)) as f32,
            release_step: (1.0 / (rate * RELEASE_SECONDS).max(1.0)) as f32,
            hold_frames,
            hold_remaining: hold_frames,
        }
    }

    /// 改变阈值，保留当前的开合状态（换拓扑时接过旧状态后调用）。
    pub fn set_threshold(&mut self, threshold_db: Option<f32>) {
        self.threshold = threshold_db.map(db_to_linear);
    }

    pub fn is_enabled(&self) -> bool {
        self.threshold.is_some()
    }

    /// 当前的增益（0 关、1 开）。
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// 输入一帧立体声的峰值检测，返回这一帧应乘的增益，并前进一帧。
    pub fn next_gain(&mut self, left: f32, right: f32) -> f32 {
        let above = match self.threshold {
            None => true,
            Some(threshold) => left.abs().max(right.abs()) >= threshold,
        };
        if above {
            self.hold_remaining = self.hold_frames;
            self.gain = (self.gain + self.attack_step).min(OPEN);
        } else if self.hold_remaining > 0 {
            self.hold_remaining -= 1;
            self.gain = (self.gain + self.attack_step).min(OPEN);
        } else {
            self.gain = (self.gain - self.release_step).max(CLOSED);
        }
        self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const LOUD: f32 = 0.5;
    /// 约 −60 dBFS，低于测试用的 −40 dBFS 阈值
    const QUIET: f32 = 0.001;
    const THRESHOLD_DB: f32 = -40.0;

    fn frames(seconds: f64) -> usize {
        (f64::from(RATE) * seconds) as usize
    }

    fn feed(gate: &mut Gate, level: f32, count: usize) -> Vec<f32> {
        (0..count).map(|_| gate.next_gain(level, -level)).collect()
    }

    #[test]
    fn a_disabled_gate_is_always_open() {
        let mut gate = Gate::new(RATE, None);
        assert!(feed(&mut gate, 0.0, frames(1.0)).iter().all(|&gain| gain == OPEN));
    }

    #[test]
    fn holds_then_releases_below_the_threshold() {
        let mut gate = Gate::new(RATE, Some(THRESHOLD_DB));
        let gains = feed(&mut gate, QUIET, frames(HOLD_SECONDS + RELEASE_SECONDS + 0.01));
        let hold = frames(HOLD_SECONDS);
        assert!(
            gains[..hold].iter().all(|&gain| gain == OPEN),
            "stays open while holding"
        );
        assert!(gains.windows(2).all(|pair| pair[1] <= pair[0]), "closes monotonically");
        assert_eq!(*gains.last().unwrap(), CLOSED);
    }

    #[test]
    fn opens_quickly_without_jumping() {
        let mut gate = Gate::new(RATE, Some(THRESHOLD_DB));
        feed(&mut gate, QUIET, frames(1.0));
        assert_eq!(gate.gain(), CLOSED);
        let gains = feed(&mut gate, LOUD, frames(ATTACK_SECONDS) + 1);
        let max_step = gains.windows(2).map(|pair| pair[1] - pair[0]).fold(0.0, f32::max);
        assert!(max_step < 0.05, "the attack is a ramp, not a jump (step {max_step})");
        assert_eq!(*gains.last().unwrap(), OPEN);
    }

    #[test]
    fn the_louder_channel_keeps_both_open() {
        let mut gate = Gate::new(RATE, Some(THRESHOLD_DB));
        for _ in 0..frames(1.0) {
            gate.next_gain(QUIET, LOUD);
        }
        assert_eq!(gate.gain(), OPEN);
    }

    #[test]
    fn changing_the_threshold_keeps_the_current_state() {
        let mut gate = Gate::new(RATE, Some(THRESHOLD_DB));
        feed(&mut gate, QUIET, frames(1.0));
        gate.set_threshold(None);
        assert_eq!(gate.gain(), CLOSED);
        let gains = feed(&mut gate, QUIET, frames(ATTACK_SECONDS) + 1);
        assert_eq!(
            *gains.last().unwrap(),
            OPEN,
            "turning the gate off reopens with the attack ramp"
        );
    }
}
