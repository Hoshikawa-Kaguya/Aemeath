//! 增益：dB 换算，以及逐样本平滑的增益（避免调节、开关连线时爆音）。

/// dB 换算为线性倍数。
pub fn db_to_linear(db: f32) -> f32 {
    const DB_PER_DECADE: f32 = 20.0;
    10f32.powf(db / DB_PER_DECADE)
}

/// 线性增益，目标值改变后在固定的帧数内线性过渡过去。
///
/// 实时线程中使用：不分配内存，`set_target` 和 `next` 都是常数时间。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmoothedGain {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl SmoothedGain {
    /// 以 `value` 为当前值和目标值，不需要过渡。
    pub fn fixed(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
        }
    }

    /// 从当前值出发，在 `ramp_frames` 帧内线性过渡到 `target`。
    pub fn set_target(&mut self, target: f32, ramp_frames: u32) {
        self.target = target;
        if ramp_frames == 0 || target == self.current {
            self.current = target;
            self.remaining = 0;
            self.step = 0.0;
        } else {
            self.remaining = ramp_frames;
            self.step = (target - self.current) / ramp_frames as f32;
        }
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    /// 已经停在 0：处理时可以整段跳过
    pub fn is_silent(&self) -> bool {
        self.remaining == 0 && self.current == 0.0
    }

    /// 渐变已经走完，增益停在目标值。
    pub fn is_settled(&self) -> bool {
        self.remaining == 0
    }

    /// 返回这一帧用的增益，并前进一帧。
    pub fn next_gain(&mut self) -> f32 {
        if self.remaining == 0 {
            return self.current;
        }
        self.remaining -= 1;
        self.current = if self.remaining == 0 {
            self.target
        } else {
            self.current + self.step
        };
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_decibels() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_linear(-6.0) - 0.501_187).abs() < 1e-5);
        assert!((db_to_linear(20.0) - 10.0).abs() < 1e-4);
    }

    #[test]
    fn ramps_linearly_and_lands_exactly_on_the_target() {
        let mut gain = SmoothedGain::fixed(0.0);
        gain.set_target(1.0, 4);
        let values: Vec<f32> = (0..6).map(|_| gain.next_gain()).collect();
        assert_eq!(values, vec![0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
        assert!(gain.is_settled());
    }

    #[test]
    fn retargeting_mid_ramp_starts_from_the_current_value() {
        let mut gain = SmoothedGain::fixed(1.0);
        gain.set_target(0.0, 4);
        gain.next_gain();
        gain.next_gain();
        gain.set_target(1.0, 2);
        assert_eq!(gain.next_gain(), 0.75);
        assert_eq!(gain.next_gain(), 1.0);
    }

    #[test]
    fn reports_silence_only_after_the_ramp() {
        let mut gain = SmoothedGain::fixed(1.0);
        gain.set_target(0.0, 2);
        assert!(!gain.is_silent());
        gain.next_gain();
        gain.next_gain();
        assert!(gain.is_silent());
    }
}
