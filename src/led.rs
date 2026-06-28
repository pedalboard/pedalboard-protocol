//! LED ring renderer: spatial patterns + temporal modifiers.
//!
//! Design: the SK6812 LEDs handle brightness natively via RGB values.
//! The renderer produces a 12-pixel frame, then the modifier attenuates it per tick.

use serde::{Deserialize, Serialize};

pub const LEDS_PER_RING: usize = 12;
pub type RingFrame = [Rgb; LEDS_PER_RING];

/// RGB color (matches smart_leds::RGB8 layout).
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const BLACK: Self = Self::new(0, 0, 0);

    /// Scale brightness by factor 0..=255 (255 = full).
    pub fn scale(self, factor: u8) -> Self {
        Self {
            r: ((self.r as u16 * factor as u16) / 255) as u8,
            g: ((self.g as u16 * factor as u16) / 255) as u8,
            b: ((self.b as u16 * factor as u16) / 255) as u8,
        }
    }
}

/// Spatial pattern — which LEDs are lit and what color.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Renderer {
    Off,
    /// All 12 LEDs same color.
    Solid(Rgb),
    /// Arc from rotation anchor, `count` LEDs lit (0–12).
    Fill(Rgb, u8),
    /// Potentiometer arc (7h→5h), blue→green→red. `fill` = 0–12.
    Heatmap(u8),
    /// Single LED at clock position (0–11).
    Single(Rgb, u8),
    /// N evenly-spaced LEDs (1,2,3,4,6,12).
    Dots(Rgb, u8),
}

/// Temporal modifier — how rendered pixels change over time.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Modifier {
    /// No modulation, full brightness.
    #[default]
    Solid,
    /// Static dim (1/4 intensity) — "available but inactive".
    Glow,
    /// On/off toggle at ~4Hz.
    Blink,
    /// Sine-wave fade, period ~1.5s.
    Pulse,
    /// Rotate pattern clockwise, one step per ~100ms.
    Rotate,
    /// Cycle hue over time (ignores original color, keeps lit/unlit pattern).
    ColorCycle,
}

/// Complete ring animation state.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RingAnimation {
    pub renderer: Renderer,
    pub modifier: Modifier,
}

impl Default for RingAnimation {
    fn default() -> Self {
        Self {
            renderer: Renderer::Off,
            modifier: Modifier::Solid,
        }
    }
}

impl RingAnimation {
    pub const fn off() -> Self {
        Self {
            renderer: Renderer::Off,
            modifier: Modifier::Solid,
        }
    }

    pub const fn solid(color: Rgb) -> Self {
        Self {
            renderer: Renderer::Solid(color),
            modifier: Modifier::Solid,
        }
    }

    pub const fn glow(color: Rgb) -> Self {
        Self {
            renderer: Renderer::Solid(color),
            modifier: Modifier::Glow,
        }
    }
}

/// Stateful ring that tracks tick for temporal modifiers.
#[derive(Copy, Clone)]
pub struct LedRing {
    pub animation: RingAnimation,
    rotation: u8,
    tick: u16,
}

impl LedRing {
    pub const fn new(rotation: u8) -> Self {
        Self {
            animation: RingAnimation {
                renderer: Renderer::Off,
                modifier: Modifier::Solid,
            },
            rotation,
            tick: 0,
        }
    }

    pub fn set(&mut self, anim: RingAnimation) {
        if self.animation != anim {
            self.animation = anim;
            self.tick = 0;
        }
    }

    /// Advance one frame. Call at 50Hz.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    /// Render current frame.
    pub fn render(&self) -> RingFrame {
        let base = self.render_spatial();
        self.apply_modifier(base)
    }

    fn render_spatial(&self) -> RingFrame {
        match self.animation.renderer {
            Renderer::Off => [Rgb::BLACK; LEDS_PER_RING],
            Renderer::Solid(c) => [c; LEDS_PER_RING],
            Renderer::Fill(c, count) => {
                let mut frame = [Rgb::BLACK; LEDS_PER_RING];
                for i in 0..(count as usize).min(LEDS_PER_RING) {
                    frame[(self.rotation as usize + LEDS_PER_RING - i) % LEDS_PER_RING] = c;
                }
                frame
            }
            Renderer::Heatmap(fill) => {
                const ARC_HOURS: [usize; 11] = [7, 8, 9, 10, 11, 0, 1, 2, 3, 4, 5];
                let mut frame = [Rgb::BLACK; LEDS_PER_RING];
                let lit = ((fill as usize) * ARC_HOURS.len() / 12).min(ARC_HOURS.len());
                for i in 0..lit {
                    frame[CLOCK[ARC_HOURS[i]]] = heatmap_color(i, ARC_HOURS.len());
                }
                frame
            }
            Renderer::Single(c, pos) => {
                let mut frame = [Rgb::BLACK; LEDS_PER_RING];
                frame[CLOCK[(pos as usize) % 12]] = c;
                frame
            }
            Renderer::Dots(c, count) => {
                let mut frame = [Rgb::BLACK; LEDS_PER_RING];
                let n = (count as usize).clamp(1, 12);
                let spacing = LEDS_PER_RING / n;
                for i in 0..n {
                    frame[CLOCK[(i * spacing) % 12]] = c;
                }
                frame
            }
        }
    }

    fn apply_modifier(&self, mut frame: RingFrame) -> RingFrame {
        match self.animation.modifier {
            Modifier::Solid => frame,
            Modifier::Glow => {
                for (i, px) in frame.iter_mut().enumerate() {
                    if i % 2 == 1 {
                        *px = Rgb::BLACK;
                    } else {
                        *px = px.scale(32); // 1/8 brightness + every 2nd LED off
                    }
                }
                frame
            }
            Modifier::Blink => {
                // ~4Hz at 50Hz tick rate: 12 ticks on, 12 off
                if (self.tick / 12) % 2 == 1 {
                    [Rgb::BLACK; LEDS_PER_RING]
                } else {
                    frame
                }
            }
            Modifier::Pulse => {
                // Sine-wave period ~75 ticks (1.5s at 50Hz)
                let phase = self.tick % 75;
                let factor = sine_u8(phase, 75);
                for px in &mut frame {
                    *px = px.scale(factor);
                }
                frame
            }
            Modifier::Rotate => {
                // Shift one position every 5 ticks (10Hz rotation)
                let shift = (self.tick / 5) as usize % LEDS_PER_RING;
                let mut rotated = [Rgb::BLACK; LEDS_PER_RING];
                for i in 0..LEDS_PER_RING {
                    rotated[(i + shift) % LEDS_PER_RING] = frame[i];
                }
                rotated
            }
            Modifier::ColorCycle => {
                let hue = (self.tick * 3) as u8;
                let color = hue_to_rgb(hue);
                for px in frame.iter_mut() {
                    if *px != Rgb::BLACK {
                        *px = color;
                    }
                }
                frame
            }
        }
    }
}

impl Default for LedRing {
    fn default() -> Self {
        Self::new(8)
    }
}

// --- Lookup tables ---

/// Physical LED index for each clock-hour position.
/// PCB layout: D1=3h, D2=2h, ..., D12=4h. 0-based.
const CLOCK: [usize; 12] = [
    3,  //  0: 12h
    2,  //  1:  1h
    1,  //  2:  2h
    0,  //  3:  3h
    11, //  4:  4h
    10, //  5:  5h
    9,  //  6:  6h
    8,  //  7:  7h
    7,  //  8:  8h
    6,  //  9:  9h
    5,  // 10: 10h
    4,  // 11: 11h
];

/// Blue (0) → Green (mid) → Red (max-1)
fn heatmap_color(pos: usize, max: usize) -> Rgb {
    if max <= 1 {
        return Rgb::new(0, 0, 255);
    }
    let t = (pos * 255) / (max - 1);
    if t < 128 {
        let g = (t * 2) as u8;
        Rgb::new(0, g, 255 - g)
    } else {
        let r = ((t - 128) * 2) as u8;
        Rgb::new(r, 255 - r, 0)
    }
}

/// HSV hue (0–255) to RGB at full saturation/value.
fn hue_to_rgb(h: u8) -> Rgb {
    let region = h / 43;
    let remainder = (h % 43) * 6;
    match region {
        0 => Rgb::new(255, remainder, 0),
        1 => Rgb::new(255 - remainder, 255, 0),
        2 => Rgb::new(0, 255, remainder),
        3 => Rgb::new(0, 255 - remainder, 255),
        4 => Rgb::new(remainder, 0, 255),
        _ => Rgb::new(255, 0, 255 - remainder),
    }
}

/// Approximate sine: maps phase (0..period) to 0..255.
fn sine_u8(phase: u16, period: u16) -> u8 {
    // Quarter-wave lookup, 16 entries
    const QUARTER: [u8; 16] = [
        0, 25, 50, 74, 98, 120, 142, 162, 180, 197, 213, 226, 237, 245, 251, 255,
    ];
    let half = period / 2;
    let quarter = period / 4;
    let pos = phase % period;
    let (idx_base, len) = if pos < quarter {
        (pos, quarter)
    } else if pos < half {
        (half - pos - 1, quarter)
    } else if pos < half + quarter {
        (pos - half, quarter)
    } else {
        (period - pos - 1, quarter)
    };
    let table_idx = (idx_base as usize * 15) / len.max(1) as usize;
    let val = QUARTER[table_idx.min(15)];
    if pos < half {
        val
    } else {
        // Second half: invert for full sine (but we want 0→255→0, not negative)
        // Actually for pulse we want 0→255→0, so half-sine:
        val
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_renders_black() {
        let ring = LedRing::default();
        let frame = ring.render();
        assert!(frame.iter().all(|px| *px == Rgb::BLACK));
    }

    #[test]
    fn solid_renders_all_same() {
        let mut ring = LedRing::default();
        let c = Rgb::new(255, 0, 0);
        ring.set(RingAnimation::solid(c));
        let frame = ring.render();
        assert!(frame.iter().all(|px| *px == c));
    }

    #[test]
    fn glow_dims_and_alternates() {
        let mut ring = LedRing::default();
        ring.set(RingAnimation::glow(Rgb::new(255, 255, 255)));
        let frame = ring.render();
        // Even indices: dimmed. Odd indices: black.
        assert_eq!(frame[0], Rgb::new(32, 32, 32));
        assert_eq!(frame[1], Rgb::BLACK);
        assert_eq!(frame[2], Rgb::new(32, 32, 32));
        assert_eq!(frame[3], Rgb::BLACK);
    }

    #[test]
    fn blink_alternates() {
        let mut ring = LedRing::default();
        let c = Rgb::new(255, 0, 0);
        ring.set(RingAnimation {
            renderer: Renderer::Solid(c),
            modifier: Modifier::Blink,
        });
        // tick 0: on
        assert!(ring.render().iter().all(|px| *px == c));
        // advance 12 ticks: off
        for _ in 0..12 {
            ring.tick();
        }
        assert!(ring.render().iter().all(|px| *px == Rgb::BLACK));
    }

    #[test]
    fn wings_2_lights_opposing() {
        let mut ring = LedRing::default();
        let c = Rgb::new(0, 255, 0);
        ring.set(RingAnimation {
            renderer: Renderer::Dots(c, 2),
            modifier: Modifier::Solid,
        });
        let frame = ring.render();
        let lit = frame.iter().filter(|px| **px != Rgb::BLACK).count();
        assert_eq!(lit, 2);
    }

    #[test]
    fn wings_3_lights_three() {
        let mut ring = LedRing::default();
        ring.set(RingAnimation {
            renderer: Renderer::Dots(Rgb::new(0, 0, 255), 3),
            modifier: Modifier::Solid,
        });
        let frame = ring.render();
        let lit = frame.iter().filter(|px| **px != Rgb::BLACK).count();
        assert_eq!(lit, 3);
    }

    #[test]
    fn heatmap_full_lights_11() {
        let mut ring = LedRing::default();
        ring.set(RingAnimation {
            renderer: Renderer::Heatmap(12),
            modifier: Modifier::Solid,
        });
        let frame = ring.render();
        let lit = frame.iter().filter(|px| **px != Rgb::BLACK).count();
        assert_eq!(lit, 11);
    }

    #[test]
    fn rotate_shifts_single() {
        let mut ring = LedRing::default();
        let c = Rgb::new(255, 255, 0);
        ring.set(RingAnimation {
            renderer: Renderer::Single(c, 0),
            modifier: Modifier::Rotate,
        });
        let f0 = ring.render();
        for _ in 0..5 {
            ring.tick();
        }
        let f1 = ring.render();
        // Pattern should have shifted
        assert_ne!(f0, f1);
    }

    #[test]
    fn pulse_starts_dim() {
        let mut ring = LedRing::default();
        ring.set(RingAnimation {
            renderer: Renderer::Solid(Rgb::new(255, 255, 255)),
            modifier: Modifier::Pulse,
        });
        // tick=0, phase=0 → sine starts at 0
        let frame = ring.render();
        assert!(frame[0].r < 10);
    }

    #[test]
    #[test]
    fn scale_zero_is_black() {
        assert_eq!(Rgb::new(255, 128, 64).scale(0), Rgb::BLACK);
    }

    #[test]
    fn scale_255_is_identity() {
        let c = Rgb::new(200, 100, 50);
        assert_eq!(c.scale(255), c);
    }

    #[test]
    fn struct_sizes() {
        assert_eq!(core::mem::size_of::<Rgb>(), 3);
        assert_eq!(core::mem::size_of::<RingAnimation>(), 6);
        assert!(core::mem::size_of::<LedRing>() <= 12);
    }
}
