//! Pedalboard configuration types shared between CLI and firmware.
//!
//! **IMPORTANT:** When changing `Preset`, `ButtonConfig`, `EncoderConfig`, `AnalogConfig`,
//! `Action`, or any type serialized into flash, bump `PRESET_SCHEMA_VERSION` below.
//! The firmware uses this to reject stale presets on boot.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Bump when any struct that is postcard-serialized into preset flash changes layout.
/// Must match `FORMAT_VERSION` in `pedalboard-midi/src/preset_format.rs`.
pub const PRESET_SCHEMA_VERSION: u8 = 5;

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
    /// Expression pedal 1 ADC value at heel (rest) position.
    #[serde(default)]
    pub exp1_min: u16,
    /// Expression pedal 1 ADC value at toe (full) position.
    #[serde(default = "default_adc_max")]
    pub exp1_max: u16,
    /// Expression pedal 2 ADC value at heel (rest) position.
    #[serde(default)]
    pub exp2_min: u16,
    /// Expression pedal 2 ADC value at toe (full) position.
    #[serde(default = "default_adc_max")]
    pub exp2_max: u16,
}

fn default_true() -> bool {
    true
}

fn default_bpm() -> u16 {
    120
}

fn default_adc_max() -> u16 {
    3750
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
            exp1_min: 0,
            exp1_max: 3750,
            exp2_min: 0,
            exp2_max: 3750,
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
    /// Incoming MIDI triggers: react to external messages by changing state or firing actions.
    #[serde(default)]
    pub triggers: Vec<Trigger, MAX_TRIGGERS>,
}

pub const MAX_TRIGGERS: usize = 8;

/// A trigger that reacts to incoming MIDI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// What incoming MIDI message to match.
    pub match_msg: TriggerMatch,
    /// What to do when matched.
    pub action: TriggerAction,
}

/// Incoming MIDI message pattern to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerMatch {
    /// Match CC on channel, with optional value range.
    Cc {
        cc: u8,
        channel: u8,
        #[serde(default)]
        value_min: u8,
        #[serde(default = "default_value_max")]
        value_max: u8,
    },
    /// Match Program Change on channel.
    ProgramChange { program: u8, channel: u8 },
    /// Match Note On on channel.
    NoteOn { note: u8, channel: u8 },
}

fn default_value_max() -> u8 {
    127
}

/// Action to perform when a trigger matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerAction {
    /// Set button active (LED on, no outgoing MIDI).
    Activate(u8),
    /// Set button inactive (LED off, no outgoing MIDI).
    Deactivate(u8),
    /// Switch to a preset by index.
    PresetSelect(u8),
    /// Fire button's on_press actions as if pressed.
    Execute(u8),
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
    /// Reactive LED: ring shows heatmap proportional to incoming CC value.
    #[serde(default)]
    pub listen_cc: Option<ListenCc>,
}

/// Reactive CC binding: maps incoming MIDI CC to LED ring visualization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenCc {
    pub cc: u8,
    pub channel: u8,
    /// Visualization mode (default: Heatmap).
    #[serde(default)]
    pub mode: ListenMode,
    /// Threshold for trigger mode (default: 64). Value ≥ threshold = on.
    #[serde(default = "default_threshold")]
    pub threshold: u8,
}

fn default_threshold() -> u8 {
    64
}

/// How the LED ring reacts to incoming CC.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListenMode {
    /// Fill proportional to value (0-127 → 0-12 LEDs).
    #[default]
    Heatmap,
    /// On/off using button's color+animation when value ≥ threshold.
    Trigger,
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
    /// Raw MIDI message (1-3 bytes, pre-encoded by CLI).
    /// Use `midi2::BytesMessage::try_from(&data[..len])` for structured debug output.
    Midi { data: [u8; 3], len: u8 },
    /// CC cycling through button's cycle_values list on each press (stateful)
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

impl Action {
    /// Control Change (status 0xBn).
    /// Returns `None` if cc > 127, value > 127, or channel not in 1..=16.
    pub fn cc(cc: u8, value: u8, channel: u8) -> Option<Self> {
        if cc > 127 || value > 127 || channel == 0 || channel > 16 {
            return None;
        }
        Some(Self::Midi {
            data: [0xB0 | (channel - 1), cc, value],
            len: 3,
        })
    }
    /// Program Change (status 0xCn).
    /// Returns `None` if program > 127 or channel not in 1..=16.
    pub fn program_change(program: u8, channel: u8) -> Option<Self> {
        if program > 127 || channel == 0 || channel > 16 {
            return None;
        }
        Some(Self::Midi {
            data: [0xC0 | (channel - 1), program, 0],
            len: 2,
        })
    }
    /// Note On (status 0x9n, velocity 127).
    /// Returns `None` if note > 127 or channel not in 1..=16.
    pub fn note_on(note: u8, channel: u8) -> Option<Self> {
        if note > 127 || channel == 0 || channel > 16 {
            return None;
        }
        Some(Self::Midi {
            data: [0x90 | (channel - 1), note, 127],
            len: 3,
        })
    }
    /// Note Off (status 0x8n, velocity 0).
    /// Returns `None` if note > 127 or channel not in 1..=16.
    pub fn note_off(note: u8, channel: u8) -> Option<Self> {
        if note > 127 || channel == 0 || channel > 16 {
            return None;
        }
        Some(Self::Midi {
            data: [0x80 | (channel - 1), note, 0],
            len: 3,
        })
    }
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
                                let _ = a.push(Action::program_change(0, 2).unwrap());
                                let _ = a.push(Action::SetLed {
                                    color: Color::Blue,
                                    animation: LedAnimation::Solid,
                                });
                                a
                            },
                            on_release: Vec::new(),
                            on_long_press: Vec::new(),
                            cycle_values: Vec::new(),
                            listen_cc: None,
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
                    triggers: Vec::new(),
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
                let _ = a.push(Action::program_change(0, 1).unwrap());
                let _ = a.push(Action::cc(69, 127, 1).unwrap());
                let _ = a.push(Action::Delay(50));
                let _ = a.push(Action::cc(70, 0, 1).unwrap());
                a
            },
            on_release: Vec::new(),
            on_long_press: {
                let mut a = Vec::new();
                let _ = a.push(Action::PresetNext);
                a
            },
            cycle_values: Vec::new(),
            listen_cc: None,
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
            exp1_min: 180,
            exp1_max: 3700,
            exp2_min: 200,
            exp2_max: 3750,
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
        // 5 bools (5B) + bpm varint (1B) + 4x u16 varint (1+2+1+2 = 6B) = 12 bytes
        assert_eq!(bytes.len(), 12);
    }

    #[test]
    fn action_cc_validates_ranges() {
        assert!(Action::cc(0, 0, 1).is_some());
        assert!(Action::cc(127, 127, 16).is_some());
        // cc > 127 (impossible with u8, but value/channel can be wrong)
        assert!(Action::cc(0, 0, 0).is_none()); // channel 0
        assert!(Action::cc(0, 0, 17).is_none()); // channel 17
    }

    #[test]
    fn action_note_on_validates_ranges() {
        assert!(Action::note_on(0, 1).is_some());
        assert!(Action::note_on(127, 16).is_some());
        assert!(Action::note_on(60, 0).is_none()); // channel 0
        assert!(Action::note_on(60, 17).is_none()); // channel 17
    }

    #[test]
    fn action_program_change_validates_ranges() {
        assert!(Action::program_change(0, 1).is_some());
        assert!(Action::program_change(127, 16).is_some());
        assert!(Action::program_change(5, 0).is_none()); // channel 0
        assert!(Action::program_change(5, 17).is_none()); // channel 17
    }

    /// Snapshot test: detects serialized layout changes without a PRESET_SCHEMA_VERSION bump.
    ///
    /// If this test fails, it means the postcard-serialized representation of Preset changed.
    /// To fix:
    /// 1. Determine if the change is breaking (reorder/remove/retype) or additive (append with default)
    /// 2. If breaking: bump PRESET_SCHEMA_VERSION and update EXPECTED_SERIALIZED_LEN below
    /// 3. If additive (new #[serde(default)] field at end): just update EXPECTED_SERIALIZED_LEN
    ///
    /// The test uses a fully-populated Preset to maximize sensitivity to layout changes.
    #[test]
    fn preset_serialization_layout_is_stable() {
        // Canonical preset with all fields populated (maximizes change detection)
        let preset = Preset {
            name: Label::try_from("Stable").unwrap(),
            buttons: {
                let mut b = Vec::new();
                let _ = b.push(ButtonConfig {
                    label: Label::try_from("Btn").unwrap(),
                    color: LedConfig {
                        on: Color::Red,
                        off: Color::Off,
                        animation: LedAnimation::Blink,
                        renderer: LedRenderer::Fill,
                        renderer_param: 6,
                    },
                    mode: ButtonMode::Toggle,
                    on_press: {
                        let mut a = Vec::new();
                        let _ = a.push(Action::cc(80, 127, 1).unwrap());
                        a
                    },
                    on_release: {
                        let mut a = Vec::new();
                        let _ = a.push(Action::cc(80, 0, 1).unwrap());
                        a
                    },
                    on_long_press: {
                        let mut a = Vec::new();
                        let _ = a.push(Action::PresetNext);
                        a
                    },
                    cycle_values: {
                        let mut cv = Vec::new();
                        let _ = cv.push(0);
                        let _ = cv.push(64);
                        let _ = cv.push(127);
                        cv
                    },
                    listen_cc: Some(ListenCc {
                        cc: 100,
                        channel: 1,
                        mode: ListenMode::Trigger,
                        threshold: 64,
                    }),
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
            analog: {
                let mut a = Vec::new();
                let _ = a.push(AnalogConfig {
                    label: Label::try_from("Wah").unwrap(),
                    cc: 11,
                    channel: 1,
                    min: 0,
                    max: 127,
                });
                a
            },
            defaults: InitialState {
                button_active: {
                    let mut v = Vec::new();
                    let _ = v.push(true);
                    v
                },
                encoder_values: {
                    let mut v = Vec::new();
                    let _ = v.push(100);
                    v
                },
            },
            on_enter: {
                let mut a = Vec::new();
                let _ = a.push(Action::program_change(0, 2).unwrap());
                a
            },
            on_exit: {
                let mut a = Vec::new();
                let _ = a.push(Action::cc(123, 0, 1).unwrap());
                a
            },
            triggers: {
                let mut t = Vec::new();
                let _ = t.push(Trigger {
                    match_msg: TriggerMatch::Cc {
                        cc: 80,
                        channel: 1,
                        value_min: 64,
                        value_max: 127,
                    },
                    action: TriggerAction::Activate(0),
                });
                t
            },
        };

        let mut buf = [0u8; 512];
        let bytes = postcard::to_slice(&preset, &mut buf).unwrap();

        // FNV-1a hash of serialized bytes — detects any layout change.
        // To fix a failure:
        // 1. If breaking (reorder/remove/retype): bump PRESET_SCHEMA_VERSION
        // 2. If additive (new #[serde(default)] at end): no version bump needed
        // 3. In both cases: update EXPECTED_HASH to the value shown in the failure message
        fn fnv1a(data: &[u8]) -> u32 {
            let mut hash: u32 = 0x811c_9dc5;
            for &b in data {
                hash ^= b as u32;
                hash = hash.wrapping_mul(0x0100_0193);
            }
            hash
        }

        let hash = fnv1a(bytes);
        const EXPECTED_HASH: u32 = 0x087d_6074;
        const EXPECTED_VERSION: u8 = 5;

        assert_eq!(
            PRESET_SCHEMA_VERSION, EXPECTED_VERSION,
            "PRESET_SCHEMA_VERSION changed — update EXPECTED_VERSION and EXPECTED_HASH"
        );
        assert_eq!(
            hash,
            EXPECTED_HASH,
            "Serialized Preset layout changed (hash {:#010x} != expected {:#010x}).\n\
             If layout changed, bump PRESET_SCHEMA_VERSION in config.rs.\n\
             Then update both EXPECTED_VERSION and EXPECTED_HASH.\n\
             Serialized bytes ({} bytes): {:02x?}",
            hash,
            EXPECTED_HASH,
            bytes.len(),
            bytes
        );
    }
}
