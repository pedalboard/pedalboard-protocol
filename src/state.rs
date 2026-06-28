//! Per-preset runtime state: tracks toggle/cycle/encoder state per preset
//! and generates recall MIDI on preset switch.

use crate::action::{action_to_midi, MidiMessage};
use crate::config::{EncoderAction, Preset, MAX_PRESETS};

const NUM_BUTTONS: usize = 6;
const NUM_ENCODERS: usize = 2;

/// Runtime state for a single preset (not persisted across power cycles).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetState {
    pub button_active: [bool; NUM_BUTTONS],
    pub cycle_index: [u8; NUM_BUTTONS],
    pub encoder_values: [u8; NUM_ENCODERS],
}

impl Default for PresetState {
    fn default() -> Self {
        Self {
            button_active: [false; NUM_BUTTONS],
            cycle_index: [0; NUM_BUTTONS],
            encoder_values: [0; NUM_ENCODERS],
        }
    }
}

/// Manages per-preset state and generates recall MIDI on switch.
#[derive(Clone)]
pub struct PresetStateStore {
    states: [PresetState; MAX_PRESETS],
    active: u8,
}

impl Default for PresetStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetStateStore {
    pub fn new() -> Self {
        Self {
            states: core::array::from_fn(|_| PresetState::default()),
            active: 0,
        }
    }

    /// Get a reference to the current active preset's state.
    pub fn current(&self) -> &PresetState {
        &self.states[self.active as usize]
    }

    /// Get a mutable reference to the current active preset's state.
    pub fn current_mut(&mut self) -> &mut PresetState {
        &mut self.states[self.active as usize]
    }

    /// Current active preset index.
    pub fn active_index(&self) -> u8 {
        self.active
    }

    /// Save working state into the active preset slot (for serialization without switching).
    pub fn save_working(&mut self, working: &PresetState) {
        self.states[self.active as usize] = working.clone();
    }

    /// Switch to a new preset. Saves current working state, loads new state,
    /// and returns MIDI messages to recall the new preset's state to external gear.
    pub fn switch(
        &mut self,
        new_preset: u8,
        working: &mut PresetState,
        preset: &Preset,
    ) -> heapless::Vec<MidiMessage, 16> {
        let mut recall = heapless::Vec::new();

        // Save current working state
        self.states[self.active as usize] = working.clone();

        // Load new state into working
        self.active = new_preset;
        *working = self.states[new_preset as usize].clone();

        // Recall: send MIDI state to external gear
        for (i, btn) in preset.buttons.iter().enumerate() {
            if working.button_active[i] {
                for action in &btn.on_press {
                    if let Some(msg) = action_to_midi(action) {
                        recall.push(msg).ok();
                    }
                }
            } else if !btn.on_release.is_empty() {
                for action in &btn.on_release {
                    if let Some(msg) = action_to_midi(action) {
                        recall.push(msg).ok();
                    }
                }
            }
        }

        // Recall encoder values
        for (i, enc) in preset.encoders.iter().enumerate() {
            if let EncoderAction::Cc { cc, channel, .. } = &enc.action {
                recall
                    .push(MidiMessage {
                        data: [0xB0 | (channel - 1), *cc as u8, working.encoder_values[i]],
                        len: 3,
                    })
                    .ok();
            }
        }

        recall
    }
}

// --- EEPROM persistence (AT24CS01: 128 bytes) ---

const EEPROM_MAGIC: u8 = 0xED;
const EEPROM_HEADER_SIZE: usize = 2; // magic + active_preset
const PRESET_STATE_SIZE: usize = 14; // 6 bools + 6 cycle + 2 encoder
/// Maximum presets that fit in 128 bytes
pub const EEPROM_MAX_PRESETS: usize = (128 - EEPROM_HEADER_SIZE) / PRESET_STATE_SIZE; // = 9

impl PresetState {
    /// Serialize to a 14-byte buffer.
    pub fn to_bytes(&self, buf: &mut [u8; PRESET_STATE_SIZE]) {
        for (i, &active) in self.button_active.iter().enumerate() {
            buf[i] = active as u8;
        }
        buf[NUM_BUTTONS..NUM_BUTTONS * 2].copy_from_slice(&self.cycle_index);
        buf[12] = self.encoder_values[0];
        buf[13] = self.encoder_values[1];
    }

    /// Deserialize from a 14-byte buffer.
    pub fn from_bytes(buf: &[u8; PRESET_STATE_SIZE]) -> Self {
        let mut state = Self::default();
        for (i, &b) in buf[..NUM_BUTTONS].iter().enumerate() {
            state.button_active[i] = b != 0;
        }
        state.cycle_index.copy_from_slice(&buf[NUM_BUTTONS..NUM_BUTTONS * 2]);
        state.encoder_values[0] = buf[12];
        state.encoder_values[1] = buf[13];
        state
    }
}

impl PresetStateStore {
    /// Serialize entire store to EEPROM buffer (128 bytes).
    /// Layout: [magic][active_preset][state0..stateN]
    pub fn to_eeprom(&self, buf: &mut [u8; 128]) {
        buf.fill(0xFF);
        buf[0] = EEPROM_MAGIC;
        buf[1] = self.active;
        for i in 0..EEPROM_MAX_PRESETS {
            let offset = EEPROM_HEADER_SIZE + i * PRESET_STATE_SIZE;
            let mut state_buf = [0u8; PRESET_STATE_SIZE];
            self.states[i].to_bytes(&mut state_buf);
            buf[offset..offset + PRESET_STATE_SIZE].copy_from_slice(&state_buf);
        }
    }

    /// Deserialize from EEPROM buffer. Returns None if magic doesn't match.
    pub fn from_eeprom(buf: &[u8; 128]) -> Option<Self> {
        if buf[0] != EEPROM_MAGIC {
            return None;
        }
        let mut store = Self::new();
        store.active = buf[1];
        for i in 0..EEPROM_MAX_PRESETS {
            let offset = EEPROM_HEADER_SIZE + i * PRESET_STATE_SIZE;
            let state_buf: &[u8; PRESET_STATE_SIZE] =
                buf[offset..offset + PRESET_STATE_SIZE].try_into().ok()?;
            store.states[i] = PresetState::from_bytes(state_buf);
        }
        Some(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use heapless::Vec;

    fn make_preset() -> Preset {
        let mut buttons: Vec<ButtonConfig, MAX_BUTTONS> = Vec::new();
        let mut on_press: Vec<Action, MAX_ACTIONS> = Vec::new();
        on_press
            .push(Action::Cc {
                cc: 10,
                value: 127,
                channel: 1,
            })
            .ok();
        let mut on_release: Vec<Action, MAX_ACTIONS> = Vec::new();
        on_release
            .push(Action::Cc {
                cc: 10,
                value: 0,
                channel: 1,
            })
            .ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Toggle,
                on_press,
                on_release,
                on_long_press: Vec::new(),
                cycle_values: Vec::new(),
            })
            .ok();

        let mut encoders: Vec<EncoderConfig, MAX_ENCODERS> = Vec::new();
        encoders
            .push(EncoderConfig {
                label: Label::try_from("Vol").unwrap(),
                action: EncoderAction::Cc {
                    cc: 7,
                    channel: 1,
                    min: 0,
                    max: 127,
                },
            })
            .ok();

        Preset {
            name: Label::try_from("Test").unwrap(),
            buttons,
            encoders,
            analog: Vec::new(),
        }
    }

    #[test]
    fn switch_saves_and_restores_state() {
        let preset = make_preset();
        let mut store = PresetStateStore::new();
        let mut working = PresetState::default();

        // Activate toggle in preset 0
        working.button_active[0] = true;
        working.encoder_values[0] = 80;

        // Switch to preset 1
        store.switch(1, &mut working, &preset);
        assert!(!working.button_active[0]);
        assert_eq!(working.encoder_values[0], 0);

        // Switch back to preset 0
        store.switch(0, &mut working, &preset);
        assert!(working.button_active[0]);
        assert_eq!(working.encoder_values[0], 80);
    }

    #[test]
    fn recall_sends_active_button_on_press() {
        let preset = make_preset();
        let mut store = PresetStateStore::new();
        let mut working = PresetState::default();

        // Set button active in preset 0, switch away, switch back
        working.button_active[0] = true;
        store.switch(1, &mut working, &preset);
        let recall = store.switch(0, &mut working, &preset);

        // Should contain CC 10 = 127 (on_press of active button)
        assert!(recall.iter().any(|m| m.data == [0xB0, 10, 127]));
    }

    #[test]
    fn recall_sends_inactive_button_on_release() {
        let preset = make_preset();
        let mut store = PresetStateStore::new();
        let mut working = PresetState::default();

        // Button inactive in preset 0 (default), switch away, switch back
        store.switch(1, &mut working, &preset);
        let recall = store.switch(0, &mut working, &preset);

        // Should contain CC 10 = 0 (on_release of inactive button)
        assert!(recall.iter().any(|m| m.data == [0xB0, 10, 0]));
    }

    #[test]
    fn recall_sends_encoder_cc() {
        let preset = make_preset();
        let mut store = PresetStateStore::new();
        let mut working = PresetState::default();

        working.encoder_values[0] = 64;
        store.switch(1, &mut working, &preset);
        let recall = store.switch(0, &mut working, &preset);

        // Should contain CC 7 = 64
        assert!(recall.iter().any(|m| m.data == [0xB0, 7, 64]));
    }

    #[test]
    fn eeprom_roundtrip() {
        let mut store = PresetStateStore::new();
        store.states[0].button_active[0] = true;
        store.states[0].button_active[3] = true;
        store.states[0].cycle_index[2] = 5;
        store.states[0].encoder_values[0] = 100;
        store.states[0].encoder_values[1] = 42;
        store.states[1].button_active[5] = true;
        store.active = 1;

        let mut buf = [0u8; 128];
        store.to_eeprom(&mut buf);

        let restored = PresetStateStore::from_eeprom(&buf).unwrap();
        assert_eq!(restored.active_index(), 1);
        assert!(restored.states[0].button_active[0]);
        assert!(restored.states[0].button_active[3]);
        assert_eq!(restored.states[0].cycle_index[2], 5);
        assert_eq!(restored.states[0].encoder_values[0], 100);
        assert_eq!(restored.states[0].encoder_values[1], 42);
        assert!(restored.states[1].button_active[5]);
    }

    #[test]
    fn eeprom_invalid_magic_returns_none() {
        let buf = [0xFFu8; 128];
        assert!(PresetStateStore::from_eeprom(&buf).is_none());
    }
}
