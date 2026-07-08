//! Long-press detection: distinguishes short press from long press using timestamps.
//!
//! Call `update(edge, now_ms)` on each input poll. Returns a `Gesture` when detected:
//! - `ShortPress` on release before threshold
//! - `LongPress` on hold ≥ threshold
//!
//! Platform-agnostic: firmware provides `Mono::now()`, simulator provides `Instant::now()`.

/// Default long-press threshold in milliseconds.
pub const LONG_PRESS_MS: u32 = 500;

/// Input edge from a debounced button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Activate,
    Deactivate,
}

/// Detected gesture after timing analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    ShortPress,
    LongPress,
}

/// Detects long-press gestures using absolute timestamps.
///
/// Call `update()` on each poll cycle with the current edge (if any) and the
/// current monotonic time in milliseconds.
#[derive(Debug, Clone)]
pub struct LongPressDetector {
    press_time: u32,
    active: bool,
    fired: bool,
    threshold_ms: u32,
}

impl Default for LongPressDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LongPressDetector {
    pub fn new() -> Self {
        Self {
            press_time: 0,
            active: false,
            fired: false,
            threshold_ms: LONG_PRESS_MS,
        }
    }

    /// Create a detector with a custom threshold.
    pub fn with_threshold(threshold_ms: u32) -> Self {
        Self {
            press_time: 0,
            active: false,
            fired: false,
            threshold_ms,
        }
    }

    /// Create a detector in "fired" state — a subsequent Deactivate will be suppressed.
    /// Used after preset switch to ignore stale button releases.
    pub fn new_fired() -> Self {
        Self {
            press_time: 0,
            active: false,
            fired: true,
            threshold_ms: LONG_PRESS_MS,
        }
    }

    /// Update with the current edge and timestamp.
    /// Returns a gesture when detected.
    pub fn update(&mut self, edge: Option<Edge>, now_ms: u32) -> Option<Gesture> {
        match edge {
            Some(Edge::Activate) => {
                self.active = true;
                self.press_time = now_ms;
                self.fired = false;
                None
            }
            Some(Edge::Deactivate) => {
                self.active = false;
                if self.fired {
                    // Long press already handled, suppress short press
                    None
                } else {
                    Some(Gesture::ShortPress)
                }
            }
            None if self.active && !self.fired => {
                let held_ms = now_ms.wrapping_sub(self.press_time);
                if held_ms >= self.threshold_ms {
                    self.fired = true;
                    Some(Gesture::LongPress)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Returns true while the button is held (before or after firing).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns true if the long press gesture has already fired.
    pub fn has_fired(&self) -> bool {
        self.fired
    }

    /// Returns ms elapsed since press, or 0 if not active.
    pub fn held_ms(&self, now_ms: u32) -> u32 {
        if self.active {
            now_ms.wrapping_sub(self.press_time)
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_press_on_release_before_threshold() {
        let mut det = LongPressDetector::new();
        assert_eq!(det.update(Some(Edge::Activate), 0), None);
        // Poll for 100ms — no edge
        assert_eq!(det.update(None, 100), None);
        // Release at 200ms (< 500ms threshold)
        assert_eq!(
            det.update(Some(Edge::Deactivate), 200),
            Some(Gesture::ShortPress)
        );
    }

    #[test]
    fn long_press_fires_at_threshold() {
        let mut det = LongPressDetector::new();
        det.update(Some(Edge::Activate), 1000);
        // Still under threshold
        assert_eq!(det.update(None, 1400), None);
        assert_eq!(det.update(None, 1499), None);
        // At threshold (500ms after press at t=1000)
        assert_eq!(det.update(None, 1500), Some(Gesture::LongPress));
    }

    #[test]
    fn release_after_long_press_suppressed() {
        let mut det = LongPressDetector::new();
        det.update(Some(Edge::Activate), 0);
        assert_eq!(det.update(None, 500), Some(Gesture::LongPress));
        // Release should NOT produce ShortPress
        assert_eq!(det.update(Some(Edge::Deactivate), 600), None);
    }

    #[test]
    fn no_double_fire_on_continued_hold() {
        let mut det = LongPressDetector::new();
        det.update(Some(Edge::Activate), 0);
        assert_eq!(det.update(None, 500), Some(Gesture::LongPress));
        // Continue holding — should not fire again
        assert_eq!(det.update(None, 600), None);
        assert_eq!(det.update(None, 1000), None);
        assert_eq!(det.update(None, 5000), None);
    }

    #[test]
    fn new_fired_suppresses_stale_release() {
        let mut det = LongPressDetector::new_fired();
        // Stale release after preset switch
        assert_eq!(det.update(Some(Edge::Deactivate), 100), None);
        // Next press works normally
        assert_eq!(det.update(Some(Edge::Activate), 200), None);
        assert_eq!(
            det.update(Some(Edge::Deactivate), 300),
            Some(Gesture::ShortPress)
        );
    }

    #[test]
    fn custom_threshold() {
        let mut det = LongPressDetector::with_threshold(1000);
        det.update(Some(Edge::Activate), 0);
        assert_eq!(det.update(None, 500), None); // would fire at default 500ms
        assert_eq!(det.update(None, 999), None);
        assert_eq!(det.update(None, 1000), Some(Gesture::LongPress));
    }

    #[test]
    fn held_ms_reports_duration() {
        let mut det = LongPressDetector::new();
        assert_eq!(det.held_ms(0), 0); // not active
        det.update(Some(Edge::Activate), 100);
        assert_eq!(det.held_ms(250), 150);
        assert_eq!(det.held_ms(600), 500);
    }

    #[test]
    fn wrapping_timestamp() {
        let mut det = LongPressDetector::new();
        // Press near u32::MAX
        det.update(Some(Edge::Activate), u32::MAX - 100);
        // Time wraps around
        assert_eq!(det.update(None, u32::MAX), None); // 100ms held
                                                      // After wrap: 400ms total (still under threshold)
        assert_eq!(det.update(None, 299), None); // wrapping_sub gives 400ms
                                                 // At threshold: 500ms total
        assert_eq!(det.update(None, 399), Some(Gesture::LongPress));
    }
}
