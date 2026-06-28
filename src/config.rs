//! Pedalboard configuration types shared between CLI and firmware.
//!
//! **IMPORTANT:** When changing `Preset`, `ButtonConfig`, `EncoderConfig`, `AnalogConfig`,
//! `Action`, or any type serialized into flash, bump `PRESET_SCHEMA_VERSION` below.
//! The firmware uses this to reject stale presets on boot.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Bump when any struct that is postcard-serialized into preset flash changes layout.
/// Must match `FORMAT_VERSION` in `pedalboard-midi/src/preset_format.rs`.
pub const PRESET_SCHEMA_VERSION: u8 = 1;

pub const MAX_PRESETS: usize = 32;
pub const MAX_BUTTONS: usize = 6;
pub const MAX_ENCODERS: usize = 2;
pub const MAX_ANALOG: usize = 2;
pub const MAX_LABEL_LEN: usize = 16;
pub const MAX_ACTIONS: usize = 8;
pub const MAX_CYCLE_VALUES: usize = 12;

pub type Label = String<MAX_LABEL_LEN>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub presets: Vec<Preset, MAX_PRESETS>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub name: Label,
    pub buttons: Vec<ButtonConfig, MAX_BUTTONS>,
    pub encoders: Vec<EncoderConfig, MAX_ENCODERS>,
    pub analog: Vec<AnalogConfig, MAX_ANALOG>,
}

// --- Buttons ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonConfig {
    pub label: Label,
    pub color: LedConfig,
    pub mode: ButtonMode,
    pub on_press: Vec<Action, MAX_ACTIONS>,
    pub on_release: Vec<Action, MAX_ACTIONS>,
    pub on_long_press: Vec<Action, MAX_ACTIONS>,
    #[serde(default)]
    pub cycle_values: Vec<u8, MAX_CYCLE_VALUES>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonMode {
    /// Fire on_press once per press
    #[default]
    Momentary,
    /// Alternate between on_press (pos 1) and on_release (pos 2)
    Toggle,
    /// Only one button in the group can be active (others deactivate)
    RadioGroup(u8),
}

// --- Actions (Morningstar-style message list) ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Control Change
    Cc { cc: u8, value: u8, channel: u8 },
    /// Program Change
    ProgramChange { program: u8, channel: u8 },
    /// Note On (velocity 127) — released on button release for momentary
    NoteOn { note: u8, channel: u8 },
    /// Note Off
    NoteOff { note: u8, channel: u8 },
    /// CC with toggled value (sends value_a first time, value_b next)
    CcToggle {
        cc: u8,
        value_a: u8,
        value_b: u8,
        channel: u8,
    },
    /// CC cycling through button's cycle_values list on each press
    CcCycle { cc: u8, channel: u8, reverse: bool },
    /// Set LED state (for sequencing LED changes in action lists)
    SetLed {
        color: Color,
        animation: LedAnimation,
    },
    /// Delay in ms between actions in a sequence
    Delay(u16),
    /// Switch to preset by index
    PresetSelect(u8),
    /// Next preset
    PresetNext,
    /// Previous preset
    PresetPrev,
    /// Bank up (scroll preset page)
    BankUp,
    /// Bank down
    BankDown,
}

// --- Encoders ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderConfig {
    pub label: Label,
    pub action: EncoderAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncoderAction {
    Cc {
        cc: u16,
        channel: u8,
        min: u8,
        max: u8,
    },
    /// Two separate CC values for CW/CCW (e.g. relative encoding)
    CcRelative {
        cc: u8,
        channel: u8,
        increment: u8,
        decrement: u8,
    },
    PresetScroll,
}

// --- Analog (expression pedals) ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalogConfig {
    pub label: Label,
    pub cc: u8,
    pub channel: u8,
    pub min: u8,
    pub max: u8,
}

// --- LED ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedConfig {
    /// Color when active/on
    pub on: Color,
    /// Color when inactive/off (None = LED off)
    pub off: Color,
    /// Animation modifier when active (default: Solid)
    #[serde(default)]
    pub animation: LedAnimation,
    /// Spatial renderer (default: Solid — all 12 LEDs)
    #[serde(default)]
    pub renderer: LedRenderer,
    /// Renderer parameter (fill count, wing count, single position)
    #[serde(default)]
    pub renderer_param: u8,
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Off,
    Red,
    Green,
    Blue,
    Yellow,
    Cyan,
    Magenta,
    White,
    Orange,
    Purple,
    Custom(u8, u8, u8), // RGB
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedAnimation {
    #[default]
    Solid,
    Blink,
    Pulse,
    Rotate,
    ColorCycle,
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedRenderer {
    /// All 12 LEDs same color
    #[default]
    Solid,
    /// Partial arc (param = count 1-12)
    Fill,
    /// Single LED (param = clock position 0-11)
    Single,
    /// N evenly-spaced LEDs (param = count 1-6)
    Dots,
}

// --- Defaults ---

impl Default for LedConfig {
    fn default() -> Self {
        LedConfig {
            on: Color::Off,
            off: Color::Off,
            animation: LedAnimation::Solid,
            renderer: LedRenderer::Solid,
            renderer_param: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_roundtrip_morningstar_style() {
        let config = Config {
            presets: {
                let mut p = Vec::new();
                let _ = p.push(Preset {
                    name: Label::try_from("Live FX").unwrap(),
                    buttons: {
                        let mut b = Vec::new();
                        let _ = b.push(ButtonConfig {
                            label: Label::try_from("Board 1").unwrap(),
                            color: LedConfig {
                                on: Color::Blue,
                                off: Color::Off,
                                animation: LedAnimation::Solid,
                                renderer: LedRenderer::Solid,
                                renderer_param: 0,
                            },
                            mode: ButtonMode::RadioGroup(1),
                            on_press: {
                                let mut a = Vec::new();
                                let _ = a.push(Action::ProgramChange {
                                    program: 0,
                                    channel: 2,
                                });
                                let _ = a.push(Action::SetLed {
                                    color: Color::Blue,
                                    animation: LedAnimation::Solid,
                                });
                                a
                            },
                            on_release: Vec::new(),
                            on_long_press: Vec::new(),
                            cycle_values: Vec::new(),
                        });
                        b
                    },
                    encoders: {
                        let mut e = Vec::new();
                        let _ = e.push(EncoderConfig {
                            label: Label::try_from("Vol").unwrap(),
                            action: EncoderAction::Cc {
                                cc: 7,
                                channel: 1,
                                min: 0,
                                max: 127,
                            },
                        });
                        e
                    },
                    analog: Vec::new(),
                });
                p
            },
        };

        let mut buf = [0u8; 512];
        let bytes = postcard::to_slice(&config, &mut buf).unwrap();
        let decoded: Config = postcard::from_bytes(bytes).unwrap();
        assert_eq!(config, decoded);
    }

    #[test]
    fn multi_action_button() {
        // Morningstar-style: button sends PC + CC + delay + CC
        let btn = ButtonConfig {
            label: Label::try_from("Scene 1").unwrap(),
            color: LedConfig {
                on: Color::Green,
                off: Color::Off,
                animation: LedAnimation::Solid,
                renderer: LedRenderer::Solid,
                renderer_param: 0,
            },
            mode: ButtonMode::Momentary,
            on_press: {
                let mut a = Vec::new();
                let _ = a.push(Action::ProgramChange {
                    program: 0,
                    channel: 1,
                });
                let _ = a.push(Action::Cc {
                    cc: 69,
                    value: 127,
                    channel: 1,
                });
                let _ = a.push(Action::Delay(50));
                let _ = a.push(Action::Cc {
                    cc: 70,
                    value: 0,
                    channel: 1,
                });
                a
            },
            on_release: Vec::new(),
            on_long_press: {
                let mut a = Vec::new();
                let _ = a.push(Action::PresetNext);
                a
            },
            cycle_values: Vec::new(),
        };

        let mut buf = [0u8; 256];
        let bytes = postcard::to_slice(&btn, &mut buf).unwrap();
        assert!(bytes.len() < 80);
        let decoded: ButtonConfig = postcard::from_bytes(bytes).unwrap();
        assert_eq!(btn, decoded);
    }
}
