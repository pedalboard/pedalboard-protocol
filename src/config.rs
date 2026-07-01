//! Pedalboard configuration types shared between CLI and firmware.
//!
//! **IMPORTANT:** When changing `Preset`, `ButtonConfig`, `EncoderConfig`, `AnalogConfig`,
//! `Action`, or any type serialized into flash, bump `PRESET_SCHEMA_VERSION` below.
//! The firmware uses this to reject stale presets on boot.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Bump when any struct that is postcard-serialized into preset flash changes layout.
/// Must match `FORMAT_VERSION` in `pedalboard-midi/src/preset_format.rs`.
pub const PRESET_SCHEMA_VERSION: u8 = 3;

pub const MAX_PRESETS: usize = 32;
pub const MAX_BUTTONS: usize = 6;
pub const MAX_ENCODERS: usize = 2;
pub const MAX_ANALOG: usize = 2;
pub const MAX_LABEL_LEN: usize = 16;
pub const MAX_ACTIONS: usize = 8;
pub const MAX_CYCLE_VALUES: usize = 12;

pub type Label = String<MAX_LABEL_LEN>;

/// PE resource ID for global configuration (presets use 0x00..0x1F).
pub const GLOBAL_CONFIG_RESOURCE: u8 = 0x7F;

/// PE resource ID for system commands.
pub const SYSTEM_COMMAND_RESOURCE: u8 = 0x7E;

/// System command identifiers (body of PE Set to resource 0x7E).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SystemCommand {
    Reboot = 0x01,
    Bootloader = 0x02,
    FactoryReset = 0x03,
}

impl SystemCommand {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(Self::Reboot),
            0x02 => Some(Self::Bootloader),
            0x03 => Some(Self::FactoryReset),
            _ => None,
        }
    }
}

/// System-wide configuration, independent of presets.
/// Replaces OpenDeck global settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Enable DIN MIDI output for locally-generated messages.
    #[serde(default = "default_true")]
    pub din_enabled: bool,
    /// Route incoming DIN MIDI → USB MIDI out.
    #[serde(default = "default_true")]
    pub din_to_usb_thru: bool,
    /// Route incoming USB MIDI → DIN MIDI out.
    #[serde(default)]
    pub usb_to_din_thru: bool,
    /// Route incoming USB MIDI → USB MIDI out (echo).
    #[serde(default)]
    pub usb_to_usb_thru: bool,
    /// Enable MIDI Clock (0xF8) output.
    #[serde(default)]
    pub midi_clock: bool,
    /// MIDI Clock tempo in BPM (30–300).
    #[serde(default = "default_bpm")]
    pub bpm: u16,
}

fn default_true() -> bool {
    true
}

fn default_bpm() -> u16 {
    120
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            din_enabled: true,
            din_to_usb_thru: true,
            usb_to_din_thru: false,
            usb_to_usb_thru: false,
            midi_clock: false,
            bpm: 120,
        }
    }
}

impl GlobalConfig {
    /// MIDI Clock tick interval in microseconds (24 PPQ).
    pub fn tick_interval_us(&self) -> u32 {
        if self.bpm == 0 {
            return 20_833; // fallback to 120 BPM
        }
        // 60_000_000 / (bpm * 24)
        60_000_000 / (self.bpm as u32 * 24)
    }
}

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
    /// Initial state applied on first boot / after upload (before any user interaction).
    #[serde(default)]
    pub defaults: InitialState,
    /// Actions fired when this preset becomes active (on switch or boot).
    #[serde(default)]
    pub on_enter: Vec<Action, MAX_ACTIONS>,
    /// Actions fired when leaving this preset (before switching to another).
    #[serde(default)]
    pub on_exit: Vec<Action, MAX_ACTIONS>,
}

/// Default toggle/radio/encoder state for a preset on first activation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialState {
    /// Which buttons start active (true = on). Length matches buttons vec.
    #[serde(default)]
    pub button_active: Vec<bool, MAX_BUTTONS>,
    /// Initial encoder values (0-127). Length matches encoders vec.
    #[serde(default)]
    pub encoder_values: Vec<u8, MAX_ENCODERS>,
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
                    defaults: Default::default(),
                    on_enter: Vec::new(),
                    on_exit: Vec::new(),
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

    #[test]
    fn global_config_postcard_roundtrip() {
        let gc = GlobalConfig {
            din_enabled: true,
            din_to_usb_thru: true,
            usb_to_din_thru: false,
            usb_to_usb_thru: false,
            midi_clock: true,
            bpm: 140,
        };
        let mut buf = [0u8; 32];
        let bytes = postcard::to_slice(&gc, &mut buf).unwrap();
        let decoded: GlobalConfig = postcard::from_bytes(bytes).unwrap();
        assert_eq!(gc, decoded);
    }

    #[test]
    fn global_config_default_values() {
        let gc = GlobalConfig::default();
        assert!(gc.din_enabled);
        assert!(gc.din_to_usb_thru);
        assert!(!gc.usb_to_din_thru);
        assert!(!gc.usb_to_usb_thru);
        assert!(!gc.midi_clock);
        assert_eq!(gc.bpm, 120);
    }

    #[test]
    fn global_config_tick_interval_120bpm() {
        let gc = GlobalConfig {
            bpm: 120,
            ..Default::default()
        };
        // 120 BPM = 60_000_000 / (120 * 24) = 20833 µs
        assert_eq!(gc.tick_interval_us(), 20833);
    }

    #[test]
    fn global_config_tick_interval_60bpm() {
        let gc = GlobalConfig {
            bpm: 60,
            ..Default::default()
        };
        // 60 BPM = 60_000_000 / (60 * 24) = 41666 µs
        assert_eq!(gc.tick_interval_us(), 41666);
    }

    #[test]
    fn global_config_tick_interval_zero_bpm_fallback() {
        let gc = GlobalConfig {
            bpm: 0,
            ..Default::default()
        };
        assert_eq!(gc.tick_interval_us(), 20833);
    }

    #[test]
    fn global_config_compact_serialization() {
        let gc = GlobalConfig::default();
        let mut buf = [0u8; 32];
        let bytes = postcard::to_slice(&gc, &mut buf).unwrap();
        // 5 bools (1 byte each) + varint u16 (1 byte for 120) = 6 bytes
        assert_eq!(bytes.len(), 6);
    }
}
