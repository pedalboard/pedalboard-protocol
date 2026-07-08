//! Encoder acceleration: converts time intervals between detents into step multipliers.
//!
//! Faster turning → shorter intervals → more steps per detent.
//! Platform-agnostic: accepts `now_ms` timestamps.

/// Encoder acceleration state. Tracks time of last detent.
#[derive(Debug, Clone)]
pub struct EncoderAccel {
    last_detent_ms: u32,
    initialized: bool,
}

impl Default for EncoderAccel {
    fn default() -> Self {
        Self::new()
    }
}

impl EncoderAccel {
    pub fn new() -> Self {
        Self {
            last_detent_ms: 0,
            initialized: false,
        }
    }

    /// Record a detent at `now_ms` and return the number of steps to apply.
    /// First detent always returns 1 (no interval to measure yet).
    pub fn steps(&mut self, now_ms: u32) -> u8 {
        if !self.initialized {
            self.last_detent_ms = now_ms;
            self.initialized = true;
            return 1;
        }

        let interval_ms = now_ms.wrapping_sub(self.last_detent_ms);
        self.last_detent_ms = now_ms;

        accel_curve(interval_ms)
    }

    /// Reset state (e.g., on preset switch).
    pub fn reset(&mut self) {
        self.initialized = false;
    }
}

/// Acceleration curve: interval (ms) → step count.
/// Tuned for typical rotary encoders with ~20 detents/revolution.
fn accel_curve(interval_ms: u32) -> u8 {
    if interval_ms < 20 {
        8 // very fast
    } else if interval_ms < 50 {
        4 // fast
    } else if interval_ms < 100 {
        2 // moderate
    } else {
        1 // slow/normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_detent_always_one_step() {
        let mut enc = EncoderAccel::new();
        assert_eq!(enc.steps(1000), 1);
    }

    #[test]
    fn slow_turn_gives_one_step() {
        let mut enc = EncoderAccel::new();
        enc.steps(0);
        assert_eq!(enc.steps(200), 1);
        assert_eq!(enc.steps(500), 1);
    }

    #[test]
    fn moderate_turn_gives_two_steps() {
        let mut enc = EncoderAccel::new();
        enc.steps(0);
        assert_eq!(enc.steps(80), 2);
        assert_eq!(enc.steps(150), 2); // 70ms since last
    }

    #[test]
    fn fast_turn_gives_four_steps() {
        let mut enc = EncoderAccel::new();
        enc.steps(0);
        assert_eq!(enc.steps(30), 4);
    }

    #[test]
    fn very_fast_turn_gives_eight_steps() {
        let mut enc = EncoderAccel::new();
        enc.steps(0);
        assert_eq!(enc.steps(10), 8);
        assert_eq!(enc.steps(15), 8); // 5ms since last
    }

    #[test]
    fn reset_makes_next_return_one() {
        let mut enc = EncoderAccel::new();
        enc.steps(0);
        enc.steps(10); // 8 steps
        enc.reset();
        assert_eq!(enc.steps(20), 1); // first after reset
    }

    #[test]
    fn acceleration_slows_down() {
        let mut enc = EncoderAccel::new();
        enc.steps(0);
        assert_eq!(enc.steps(10), 8); // fast burst
        assert_eq!(enc.steps(110), 1); // slowed down (100ms gap)
    }

    #[test]
    fn wrapping_timestamp() {
        let mut enc = EncoderAccel::new();
        enc.steps(u32::MAX - 10);
        // 20ms later (wraps)
        assert_eq!(enc.steps(9), 4); // wrapping_sub gives 20ms → but < 20 is 8...
                                     // Actually: MAX-10 to 9 = 20ms. interval < 20 → 8 steps? No, 20 is not < 20.
                                     // interval_ms = 9 - (MAX-10) wrapping = 9 + 11 = 20. 20 < 50 → 4 steps.
    }
}
