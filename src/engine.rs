//! Preset engine: processes abstract input events against preset config.
//! Pure business logic — no hardware dependencies.

use crate::action::{action_to_midi, analog_cc, encoder_cc, EncoderDirection, MidiMessage};
use crate::config::{
    Action, ButtonMode, EncoderAction, Label, Preset, MAX_ACTIONS, MAX_CYCLE_VALUES,
};
use crate::state::PresetState;

const NUM_BUTTONS: usize = 6;

/// Abstract button event after long-press detection is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonEvent {
    /// Short press (or immediate press if no long-press configured)
    Press,
    /// Release (for momentary mode)
    Release,
    /// Long press (held past threshold)
    LongPress,
}

/// System-level actions that transcend MIDI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAction {
    PresetNext,
    PresetPrev,
}

/// Which display to show an overlay on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySide {
    L,
    R,
}

/// Display events emitted directly from actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayEvent {
    EncoderOverlay {
        side: DisplaySide,
        label: Label,
        value: u8,
    },
    AnalogOverlay {
        side: DisplaySide,
        label: Label,
        value: u8,
    },
    /// Shown while holding a button with on_long_press (before threshold fires)
    LongPressHint { action: SystemAction },
    /// Clear the hint (button released before threshold)
    LongPressCancel,
}

/// A single step in an action sequence: either a MIDI message or a delay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionStep {
    Send(MidiMessage),
    Delay(u16),
}

/// Result of processing an input event.
pub struct EngineResult {
    pub midi: heapless::Vec<ActionStep, 8>,
    pub system: heapless::Vec<SystemAction, 2>,
    pub display: heapless::Vec<DisplayEvent, 2>,
    pub led_dirty: bool,
}

impl EngineResult {
    fn new() -> Self {
        Self {
            midi: heapless::Vec::new(),
            system: heapless::Vec::new(),
            display: heapless::Vec::new(),
            led_dirty: false,
        }
    }
}

/// Process a button event. Updates state and returns MIDI/system/display actions.
pub fn process_button(
    state: &mut PresetState,
    preset: &Preset,
    btn_idx: usize,
    event: ButtonEvent,
) -> EngineResult {
    let mut result = EngineResult::new();
    let Some(btn) = preset.buttons.get(btn_idx) else {
        return result;
    };
    let mode = &btn.mode;

    match event {
        ButtonEvent::Press => {
            match mode {
                ButtonMode::Toggle => {
                    state.button_active[btn_idx] = !state.button_active[btn_idx];
                    result.led_dirty = true;
                }
                ButtonMode::Momentary => {
                    state.button_active[btn_idx] = true;
                    result.led_dirty = true;
                }
                ButtonMode::RadioGroup(group) => {
                    for j in 0..NUM_BUTTONS {
                        if j != btn_idx {
                            if let Some(other) = preset.buttons.get(j) {
                                if other.mode == ButtonMode::RadioGroup(*group) {
                                    state.button_active[j] = false;
                                }
                            }
                        }
                    }
                    state.button_active[btn_idx] = true;
                    result.led_dirty = true;
                }
            }
            execute_actions(
                &btn.on_press,
                &btn.cycle_values,
                &mut result.midi,
                &mut result.system,
                &mut state.cycle_index[btn_idx],
            );
        }
        ButtonEvent::Release => {
            if matches!(mode, ButtonMode::Momentary) {
                state.button_active[btn_idx] = false;
                result.led_dirty = true;
            }
            execute_actions(
                &btn.on_release,
                &btn.cycle_values,
                &mut result.midi,
                &mut result.system,
                &mut state.cycle_index[btn_idx],
            );
        }
        ButtonEvent::LongPress => {
            execute_actions(
                &btn.on_long_press,
                &btn.cycle_values,
                &mut result.midi,
                &mut result.system,
                &mut state.cycle_index[btn_idx],
            );
        }
    }

    result
}

/// Process an encoder change. Applies steps, returns MIDI + display event.
pub fn process_encoder(
    state: &mut PresetState,
    preset: &Preset,
    enc_idx: usize,
    direction: EncoderDirection,
    steps: u8,
) -> EngineResult {
    let mut result = EngineResult::new();
    let Some(enc) = preset.encoders.get(enc_idx) else {
        return result;
    };

    for _ in 0..steps {
        encoder_cc(
            preset,
            enc_idx,
            direction,
            &mut state.encoder_values[enc_idx],
        );
    }

    match &enc.action {
        EncoderAction::Cc { cc, channel, .. } => {
            result
                .midi
                .push(ActionStep::Send(MidiMessage {
                    data: [
                        0xB0 | (channel - 1),
                        *cc as u8,
                        state.encoder_values[enc_idx],
                    ],
                    len: 3,
                }))
                .ok();
        }
        EncoderAction::CcRelative {
            cc,
            channel,
            increment,
            decrement,
        } => {
            let val = match direction {
                EncoderDirection::Clockwise => *increment,
                EncoderDirection::CounterClockwise => *decrement,
            };
            result
                .midi
                .push(ActionStep::Send(MidiMessage {
                    data: [0xB0 | (channel - 1), *cc, val],
                    len: 3,
                }))
                .ok();
        }
        EncoderAction::PresetScroll => match direction {
            EncoderDirection::Clockwise => {
                result.system.push(SystemAction::PresetNext).ok();
            }
            EncoderDirection::CounterClockwise => {
                result.system.push(SystemAction::PresetPrev).ok();
            }
        },
    }

    let side = if enc_idx == 0 {
        DisplaySide::L
    } else {
        DisplaySide::R
    };
    result
        .display
        .push(DisplayEvent::EncoderOverlay {
            side,
            label: enc.label.clone(),
            value: state.encoder_values[enc_idx],
        })
        .ok();
    result.led_dirty = true;

    result
}

/// Process an analog input change. Returns MIDI + display event.
pub fn process_analog(preset: &Preset, analog_idx: usize, raw: u16, adc_max: u16) -> EngineResult {
    let mut result = EngineResult::new();
    if let Some(msg) = analog_cc(preset, analog_idx, raw, adc_max) {
        let side = if analog_idx == 0 {
            DisplaySide::L
        } else {
            DisplaySide::R
        };
        let label = preset
            .analog
            .get(analog_idx)
            .map(|a| a.label.clone())
            .unwrap_or_default();
        result
            .display
            .push(DisplayEvent::AnalogOverlay {
                side,
                label,
                value: msg.data[2],
            })
            .ok();
        result.midi.push(ActionStep::Send(msg)).ok();
    }
    result
}

fn execute_actions(
    actions: &heapless::Vec<Action, MAX_ACTIONS>,
    cycle_values: &heapless::Vec<u8, MAX_CYCLE_VALUES>,
    midi: &mut heapless::Vec<ActionStep, 8>,
    system: &mut heapless::Vec<SystemAction, 2>,
    cycle_index: &mut u8,
) {
    for action in actions {
        match action {
            Action::PresetNext => {
                system.push(SystemAction::PresetNext).ok();
            }
            Action::PresetPrev => {
                system.push(SystemAction::PresetPrev).ok();
            }
            Action::Delay(ms) => {
                midi.push(ActionStep::Delay(*ms)).ok();
            }
            Action::CcCycle {
                cc,
                channel,
                reverse,
            } => {
                if !cycle_values.is_empty() {
                    let idx = (*cycle_index as usize) % cycle_values.len();
                    let value = cycle_values[idx];
                    midi.push(ActionStep::Send(MidiMessage {
                        data: [0xB0 | (channel - 1), *cc, value],
                        len: 3,
                    }))
                    .ok();
                    if *reverse {
                        *cycle_index = if *cycle_index == 0 {
                            (cycle_values.len() - 1) as u8
                        } else {
                            *cycle_index - 1
                        };
                    } else {
                        *cycle_index = ((*cycle_index as usize + 1) % cycle_values.len()) as u8;
                    }
                }
            }
            _ => {
                if let Some(msg) = action_to_midi(action) {
                    midi.push(ActionStep::Send(msg)).ok();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn make_toggle_preset() -> Preset {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press
            .push(Action::Cc {
                cc: 80,
                value: 127,
                channel: 1,
            })
            .ok();
        let mut on_release: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_release
            .push(Action::Cc {
                cc: 80,
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
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
            })
            .ok();
        Preset {
            name: Label::try_from("Test").unwrap(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
        }
    }

    #[test]
    fn toggle_press_flips_state_and_fires_on_press() {
        let preset = make_toggle_preset();
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(state.button_active[0]);
        assert!(r.led_dirty);
        assert_eq!(r.midi.len(), 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 80, 127]));
    }

    #[test]
    fn toggle_second_press_fires_on_press_again() {
        let preset = make_toggle_preset();
        let mut state = PresetState::default();

        process_button(&mut state, &preset, 0, ButtonEvent::Press);
        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(!state.button_active[0]);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 80, 127]));
    }

    #[test]
    fn momentary_press_and_release() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press
            .push(Action::NoteOn {
                note: 60,
                channel: 1,
            })
            .ok();
        let mut on_release: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_release
            .push(Action::NoteOff {
                note: 60,
                channel: 1,
            })
            .ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press,
                on_release,
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
        };
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(state.button_active[0]);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0x90, 60, 127]));

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Release);
        assert!(!state.button_active[0]);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0x80, 60, 0]));
    }

    #[test]
    fn long_press_fires_on_long_press_actions() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_long_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_long_press.push(Action::PresetNext).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press: heapless::Vec::new(),
                on_release: heapless::Vec::new(),
                on_long_press,
                cycle_values: heapless::Vec::new(),
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
        };
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::LongPress);
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.system[0], SystemAction::PresetNext);
    }

    #[test]
    fn radio_group_deactivates_others() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        for _ in 0..3 {
            buttons
                .push(ButtonConfig {
                    label: Label::new(),
                    color: LedConfig::default(),
                    mode: ButtonMode::RadioGroup(1),
                    on_press: heapless::Vec::new(),
                    on_release: heapless::Vec::new(),
                    on_long_press: heapless::Vec::new(),
                    cycle_values: heapless::Vec::new(),
                })
                .ok();
        }
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
        };
        let mut state = PresetState::default();

        process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(state.button_active[0]);

        process_button(&mut state, &preset, 1, ButtonEvent::Press);
        assert!(!state.button_active[0]);
        assert!(state.button_active[1]);
    }

    #[test]
    fn encoder_cc_increments_and_emits_display() {
        let mut encoders: heapless::Vec<EncoderConfig, MAX_ENCODERS> = heapless::Vec::new();
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
        let preset = Preset {
            name: Label::new(),
            buttons: heapless::Vec::new(),
            encoders,
            analog: heapless::Vec::new(),
            defaults: Default::default(),
        };
        let mut state = PresetState::default();
        state.encoder_values[0] = 64;

        let r = process_encoder(&mut state, &preset, 0, EncoderDirection::Clockwise, 1);
        assert_eq!(state.encoder_values[0], 65);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 7, 65]));
        assert_eq!(r.display.len(), 1);
        match &r.display[0] {
            DisplayEvent::EncoderOverlay { side, label, value } => {
                assert_eq!(*side, DisplaySide::L);
                assert_eq!(label.as_str(), "Vol");
                assert_eq!(*value, 65);
            }
            _ => panic!("expected EncoderOverlay"),
        }
    }

    #[test]
    fn delay_in_action_sequence() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press
            .push(Action::Cc {
                cc: 1,
                value: 127,
                channel: 1,
            })
            .ok();
        on_press.push(Action::Delay(50)).ok();
        on_press
            .push(Action::Cc {
                cc: 2,
                value: 0,
                channel: 1,
            })
            .ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press,
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
        };
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert_eq!(r.midi.len(), 3);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 1, 127]));
        assert_eq!(r.midi[1], ActionStep::Delay(50));
        assert!(matches!(&r.midi[2], ActionStep::Send(m) if m.data == [0xB0, 2, 0]));
    }
}
