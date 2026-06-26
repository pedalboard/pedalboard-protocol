//! Pedalboard configuration types shared between CLI and firmware.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum presets in a setlist.
pub const MAX_PRESETS: usize = 32;
pub const MAX_BUTTONS: usize = 6;
pub const MAX_ENCODERS: usize = 2;
pub const MAX_LABEL_LEN: usize = 16;

pub type Label = String<MAX_LABEL_LEN>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub presets: Vec<Preset, MAX_PRESETS>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub name: Label,
    pub buttons: Vec<ButtonConfig, MAX_BUTTONS>,
    pub encoders: Vec<EncoderConfig, MAX_ENCODERS>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonConfig {
    pub label: Label,
    pub action: ButtonAction,
    pub color: Color,
    pub behavior: Behavior,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonAction {
    None,
    Note { note: u8, channel: u8 },
    Cc { cc: u8, value: u8, channel: u8 },
    ProgramChange { program: u8, channel: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Behavior {
    Momentary,
    Toggle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderConfig {
    pub label: Label,
    pub cc: u16,
    pub channel: u8,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Off,
    Red,
    Green,
    Blue,
    Yellow,
    Cyan,
    Magenta,
    White,
}

impl Default for Color {
    fn default() -> Self {
        Color::Off
    }
}

impl Default for Behavior {
    fn default() -> Self {
        Behavior::Momentary
    }
}

impl Default for ButtonAction {
    fn default() -> Self {
        ButtonAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_roundtrip() {
        let config = Config {
            presets: {
                let mut p = Vec::new();
                p.push(Preset {
                    name: Label::try_from("Live FX").unwrap(),
                    buttons: {
                        let mut b = Vec::new();
                        b.push(ButtonConfig {
                            label: Label::try_from("Board 1").unwrap(),
                            action: ButtonAction::ProgramChange {
                                program: 0,
                                channel: 2,
                            },
                            color: Color::Blue,
                            behavior: Behavior::Momentary,
                        })
                        .unwrap();
                        b
                    },
                    encoders: {
                        let mut e = Vec::new();
                        e.push(EncoderConfig {
                            label: Label::try_from("Vol").unwrap(),
                            cc: 7,
                            channel: 1,
                        })
                        .unwrap();
                        e
                    },
                })
                .unwrap();
                p
            },
        };

        let mut buf = [0u8; 256];
        let bytes = postcard::to_slice(&config, &mut buf).unwrap();
        let decoded: Config = postcard::from_bytes(bytes).unwrap();
        assert_eq!(config, decoded);
    }

    #[test]
    fn compact_size() {
        // A single preset with one button should be small
        let config = Config {
            presets: {
                let mut p = Vec::new();
                p.push(Preset {
                    name: Label::try_from("Test").unwrap(),
                    buttons: {
                        let mut b = Vec::new();
                        b.push(ButtonConfig {
                            label: Label::try_from("A").unwrap(),
                            action: ButtonAction::Cc {
                                cc: 80,
                                value: 127,
                                channel: 1,
                            },
                            color: Color::Green,
                            behavior: Behavior::Toggle,
                        })
                        .unwrap();
                        b
                    },
                    encoders: Vec::new(),
                })
                .unwrap();
                p
            },
        };

        let mut buf = [0u8; 256];
        let bytes = postcard::to_slice(&config, &mut buf).unwrap();
        // Should be very compact — well under 50 bytes for this
        assert!(bytes.len() < 50, "got {} bytes", bytes.len());
    }
}
