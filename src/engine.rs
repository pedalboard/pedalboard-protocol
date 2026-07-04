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
    PresetSelect(u8),
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
        ButtonEvent::Press => match mode {
            ButtonMode::Toggle => {
                state.button_active[btn_idx] = !state.button_active[btn_idx];
                result.led_dirty = true;
                if state.button_active[btn_idx] {
                    execute_actions(
                        &btn.on_press,
                        &btn.cycle_values,
                        &mut result.midi,
                        &mut result.system,
                        &mut state.cycle_index[btn_idx],
                    );
                } else {
                    execute_actions(
                        &btn.on_release,
                        &btn.cycle_values,
                        &mut result.midi,
                        &mut result.system,
                        &mut state.cycle_index[btn_idx],
                    );
                }
            }
            ButtonMode::Momentary => {
                state.button_active[btn_idx] = true;
                result.led_dirty = true;
                execute_actions(
                    &btn.on_press,
                    &btn.cycle_values,
                    &mut result.midi,
                    &mut result.system,
                    &mut state.cycle_index[btn_idx],
                );
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
                execute_actions(
                    &btn.on_press,
                    &btn.cycle_values,
                    &mut result.midi,
                    &mut result.system,
                    &mut state.cycle_index[btn_idx],
                );
            }
        },
        ButtonEvent::Release => {
            if matches!(mode, ButtonMode::Momentary) {
                state.button_active[btn_idx] = false;
                result.led_dirty = true;
                execute_actions(
                    &btn.on_release,
                    &btn.cycle_values,
                    &mut result.midi,
                    &mut result.system,
                    &mut state.cycle_index[btn_idx],
                );
            }
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
pub fn process_analog(
    preset: &Preset,
    analog_idx: usize,
    raw: u16,
    adc_min: u16,
    adc_max: u16,
) -> EngineResult {
    let mut result = EngineResult::new();
    if let Some(msg) = analog_cc(preset, analog_idx, raw, adc_min, adc_max) {
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
            Action::PresetSelect(idx) => {
                system.push(SystemAction::PresetSelect(*idx)).ok();
            }
            Action::BankUp | Action::BankDown => {
                // Bank actions are PresetSelect with page arithmetic.
                // Requires runtime context (active preset, page size) not available here.
                // Handled as PresetNext/PresetPrev until bank concept is defined.
                let sa = if matches!(action, Action::BankUp) {
                    SystemAction::PresetNext
                } else {
                    SystemAction::PresetPrev
                };
                system.push(sa).ok();
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

/// Result of processing incoming MIDI against triggers.
pub struct TriggerResult {
    /// MIDI messages to send (from Execute action).
    pub midi: heapless::Vec<ActionStep, 8>,
    /// System actions (preset switch).
    pub system: heapless::Vec<SystemAction, 2>,
    /// Whether LED state changed (activate/deactivate).
    pub led_dirty: bool,
}

/// Process incoming MIDI against preset triggers. Updates button state directly.
pub fn process_triggers(
    state: &mut PresetState,
    preset: &Preset,
    status: u8,
    data1: u8,
    data2: u8,
) -> TriggerResult {
    use crate::config::{TriggerAction, TriggerMatch};

    let mut result = TriggerResult {
        midi: heapless::Vec::new(),
        system: heapless::Vec::new(),
        led_dirty: false,
    };

    let msg_type = status & 0xF0;
    let channel = (status & 0x0F) + 1;

    for trigger in &preset.triggers {
        let matched = match &trigger.match_msg {
            TriggerMatch::Cc {
                cc,
                channel: ch,
                value_min,
                value_max,
            } => {
                msg_type == 0xB0
                    && channel == *ch
                    && data1 == *cc
                    && data2 >= *value_min
                    && data2 <= *value_max
            }
            TriggerMatch::ProgramChange {
                program,
                channel: ch,
            } => msg_type == 0xC0 && channel == *ch && data1 == *program,
            TriggerMatch::NoteOn { note, channel: ch } => {
                msg_type == 0x90 && channel == *ch && data1 == *note && data2 > 0
            }
        };

        if !matched {
            continue;
        }

        match &trigger.action {
            TriggerAction::Activate(btn_idx) => {
                let idx = *btn_idx as usize;
                if idx < state.button_active.len() {
                    state.button_active[idx] = true;
                    result.led_dirty = true;
                }
            }
            TriggerAction::Deactivate(btn_idx) => {
                let idx = *btn_idx as usize;
                if idx < state.button_active.len() {
                    state.button_active[idx] = false;
                    result.led_dirty = true;
                }
            }
            TriggerAction::PresetSelect(preset_idx) => {
                result
                    .system
                    .push(SystemAction::PresetSelect(*preset_idx))
                    .ok();
            }
            TriggerAction::Execute(btn_idx) => {
                let idx = *btn_idx as usize;
                if let Some(btn) = preset.buttons.get(idx) {
                    execute_actions(
                        &btn.on_press,
                        &btn.cycle_values,
                        &mut result.midi,
                        &mut result.system,
                        &mut state.cycle_index[idx],
                    );
                }
            }
        }
    }

    result
}

/// Result of a reactive CC match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactiveResult {
    /// Heatmap fill level (0-12 LEDs).
    Heatmap(usize, u8),
    /// Trigger on/off (button index, active).
    Trigger(usize, bool),
}

/// Check incoming CC against preset's reactive LED bindings.
pub fn process_incoming_cc(
    preset: &Preset,
    channel: u8,
    cc: u8,
    value: u8,
) -> Option<ReactiveResult> {
    use crate::config::ListenMode;
    for (i, btn) in preset.buttons.iter().enumerate() {
        if let Some(listen) = &btn.listen_cc {
            if listen.cc == cc && listen.channel == channel {
                return Some(match listen.mode {
                    ListenMode::Heatmap => {
                        let fill = ((value as u16 * 12) / 127).min(12) as u8;
                        ReactiveResult::Heatmap(i, fill)
                    }
                    ListenMode::Trigger => ReactiveResult::Trigger(i, value >= listen.threshold),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn make_toggle_preset() -> Preset {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();
        let mut on_release: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_release.push(Action::cc(80, 0, 1).unwrap()).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Toggle,
                on_press,
                on_release,
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();
        Preset {
            name: Label::try_from("Test").unwrap(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
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
    fn toggle_second_press_deactivates_and_fires_on_release() {
        let preset = make_toggle_preset();
        let mut state = PresetState::default();

        process_button(&mut state, &preset, 0, ButtonEvent::Press);
        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(!state.button_active[0]);
        // Second press deactivates → fires on_release (CC#80 = 0)
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 80, 0]));
    }

    #[test]
    fn momentary_press_and_release() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press.push(Action::note_on(60, 1).unwrap()).ok();
        let mut on_release: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_release.push(Action::note_off(60, 1).unwrap()).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press,
                on_release,
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
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
                listen_cc: None,
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
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
                    listen_cc: None,
                })
                .ok();
        }
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
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
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
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
    fn encoder_cc_counter_clockwise_decrements() {
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
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();
        state.encoder_values[0] = 64;

        let r = process_encoder(
            &mut state,
            &preset,
            0,
            EncoderDirection::CounterClockwise,
            1,
        );
        assert_eq!(state.encoder_values[0], 63);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 7, 63]));
    }

    #[test]
    fn encoder_cc_multi_step_acceleration() {
        let mut encoders: heapless::Vec<EncoderConfig, MAX_ENCODERS> = heapless::Vec::new();
        encoders
            .push(EncoderConfig {
                label: Label::try_from("Gain").unwrap(),
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
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();
        state.encoder_values[0] = 50;

        // 4 steps CW: 50 → 54, single MIDI message with final value
        let r = process_encoder(&mut state, &preset, 0, EncoderDirection::Clockwise, 4);
        assert_eq!(state.encoder_values[0], 54);
        assert_eq!(r.midi.len(), 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 7, 54]));
    }

    #[test]
    fn encoder_cc_clamps_at_max() {
        let mut encoders: heapless::Vec<EncoderConfig, MAX_ENCODERS> = heapless::Vec::new();
        encoders
            .push(EncoderConfig {
                label: Label::new(),
                action: EncoderAction::Cc {
                    cc: 7,
                    channel: 1,
                    min: 0,
                    max: 100,
                },
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons: heapless::Vec::new(),
            encoders,
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();
        state.encoder_values[0] = 99;

        // 5 steps CW from 99, max=100 → clamps at 100
        let r = process_encoder(&mut state, &preset, 0, EncoderDirection::Clockwise, 5);
        assert_eq!(state.encoder_values[0], 100);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data[2] == 100));
    }

    #[test]
    fn encoder_cc_clamps_at_min() {
        let mut encoders: heapless::Vec<EncoderConfig, MAX_ENCODERS> = heapless::Vec::new();
        encoders
            .push(EncoderConfig {
                label: Label::new(),
                action: EncoderAction::Cc {
                    cc: 7,
                    channel: 1,
                    min: 20,
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
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();
        state.encoder_values[0] = 21;

        // 5 steps CCW from 21, min=20 → clamps at 20
        let r = process_encoder(
            &mut state,
            &preset,
            0,
            EncoderDirection::CounterClockwise,
            5,
        );
        assert_eq!(state.encoder_values[0], 20);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data[2] == 20));
    }

    #[test]
    fn encoder_relative_sends_increment_decrement() {
        let mut encoders: heapless::Vec<EncoderConfig, MAX_ENCODERS> = heapless::Vec::new();
        encoders
            .push(EncoderConfig {
                label: Label::try_from("FX").unwrap(),
                action: EncoderAction::CcRelative {
                    cc: 16,
                    channel: 1,
                    increment: 65,
                    decrement: 63,
                },
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons: heapless::Vec::new(),
            encoders,
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();

        let r = process_encoder(&mut state, &preset, 0, EncoderDirection::Clockwise, 1);
        assert_eq!(r.midi.len(), 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 16, 65]));

        let r = process_encoder(
            &mut state,
            &preset,
            0,
            EncoderDirection::CounterClockwise,
            1,
        );
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 16, 63]));
    }

    #[test]
    fn encoder_preset_scroll_emits_system_action() {
        let mut encoders: heapless::Vec<EncoderConfig, MAX_ENCODERS> = heapless::Vec::new();
        encoders
            .push(EncoderConfig {
                label: Label::try_from("Bank").unwrap(),
                action: EncoderAction::PresetScroll,
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons: heapless::Vec::new(),
            encoders,
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();

        let r = process_encoder(&mut state, &preset, 0, EncoderDirection::Clockwise, 1);
        assert!(r.midi.is_empty());
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.system[0], SystemAction::PresetNext);

        let r = process_encoder(
            &mut state,
            &preset,
            0,
            EncoderDirection::CounterClockwise,
            1,
        );
        assert_eq!(r.system[0], SystemAction::PresetPrev);
    }

    #[test]
    fn encoder_invalid_index_returns_empty() {
        let preset = Preset {
            name: Label::new(),
            buttons: heapless::Vec::new(),
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();

        let r = process_encoder(&mut state, &preset, 0, EncoderDirection::Clockwise, 1);
        assert!(r.midi.is_empty());
        assert!(r.system.is_empty());
        assert!(r.display.is_empty());
    }

    #[test]
    fn encoder_second_index_uses_right_display() {
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
        encoders
            .push(EncoderConfig {
                label: Label::try_from("Gain").unwrap(),
                action: EncoderAction::Cc {
                    cc: 8,
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
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();
        state.encoder_values[1] = 50;

        let r = process_encoder(&mut state, &preset, 1, EncoderDirection::Clockwise, 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 8, 51]));
        match &r.display[0] {
            DisplayEvent::EncoderOverlay { side, label, value } => {
                assert_eq!(*side, DisplaySide::R);
                assert_eq!(label.as_str(), "Gain");
                assert_eq!(*value, 51);
            }
            _ => panic!("expected EncoderOverlay"),
        }
    }

    #[test]
    fn delay_in_action_sequence() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press.push(Action::cc(1, 127, 1).unwrap()).ok();
        on_press.push(Action::Delay(50)).ok();
        on_press.push(Action::cc(2, 0, 1).unwrap()).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press,
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert_eq!(r.midi.len(), 3);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 1, 127]));
        assert_eq!(r.midi[1], ActionStep::Delay(50));
        assert!(matches!(&r.midi[2], ActionStep::Send(m) if m.data == [0xB0, 2, 0]));
    }

    #[test]
    fn incoming_cc_matches_listen_binding() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::default(),
                on_press: heapless::Vec::new(),
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: Some(ListenCc {
                    cc: 100,
                    channel: 3,
                    mode: ListenMode::default(),
                    threshold: 64,
                }),
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };

        // Matching CC
        let result = process_incoming_cc(&preset, 3, 100, 127);
        assert_eq!(result, Some(ReactiveResult::Heatmap(0, 12)));

        // Half value
        let result = process_incoming_cc(&preset, 3, 100, 64);
        assert_eq!(result, Some(ReactiveResult::Heatmap(0, 6)));

        // Zero
        let result = process_incoming_cc(&preset, 3, 100, 0);
        assert_eq!(result, Some(ReactiveResult::Heatmap(0, 0)));

        // Wrong CC
        assert_eq!(process_incoming_cc(&preset, 3, 99, 127), None);

        // Wrong channel
        assert_eq!(process_incoming_cc(&preset, 1, 100, 127), None);
    }

    #[test]
    fn incoming_cc_trigger_mode() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::default(),
                on_press: heapless::Vec::new(),
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: Some(ListenCc {
                    cc: 80,
                    channel: 2,
                    mode: ListenMode::Trigger,
                    threshold: 64,
                }),
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };

        assert_eq!(
            process_incoming_cc(&preset, 2, 80, 127),
            Some(ReactiveResult::Trigger(0, true))
        );
        assert_eq!(
            process_incoming_cc(&preset, 2, 80, 64),
            Some(ReactiveResult::Trigger(0, true))
        );
        assert_eq!(
            process_incoming_cc(&preset, 2, 80, 63),
            Some(ReactiveResult::Trigger(0, false))
        );
        assert_eq!(
            process_incoming_cc(&preset, 2, 80, 0),
            Some(ReactiveResult::Trigger(0, false))
        );
    }

    #[test]
    fn preset_select_emits_system_action() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press.push(Action::PresetSelect(5)).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press,
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.system[0], SystemAction::PresetSelect(5));
        // No MIDI output — it's a system action only
        assert!(r.midi.is_empty());
    }

    #[test]
    fn bank_up_down_emit_system_actions() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press_up: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press_up.push(Action::BankUp).ok();
        let mut on_press_down: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press_down.push(Action::BankDown).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press: on_press_up,
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press: on_press_down,
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.system[0], SystemAction::PresetNext);

        let r = process_button(&mut state, &preset, 1, ButtonEvent::Press);
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.system[0], SystemAction::PresetPrev);
    }

    // --- CcCycle tests ---

    fn make_cc_cycle_preset(reverse: bool) -> Preset {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press
            .push(Action::CcCycle {
                cc: 50,
                channel: 1,
                reverse,
            })
            .ok();
        let mut cycle_values: heapless::Vec<u8, MAX_CYCLE_VALUES> = heapless::Vec::new();
        cycle_values.push(0).ok();
        cycle_values.push(32).ok();
        cycle_values.push(64).ok();
        cycle_values.push(127).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press,
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values,
                listen_cc: None,
            })
            .ok();
        Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        }
    }

    #[test]
    fn cc_cycle_forward_iterates_values() {
        let preset = make_cc_cycle_preset(false);
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 0]));

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 32]));

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 64]));

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 127]));
    }

    #[test]
    fn cc_cycle_forward_wraps_around() {
        let preset = make_cc_cycle_preset(false);
        let mut state = PresetState::default();

        // Cycle through all 4 values
        for _ in 0..4 {
            process_button(&mut state, &preset, 0, ButtonEvent::Press);
        }
        // 5th press should wrap to first value
        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 0]));
    }

    #[test]
    fn cc_cycle_reverse_iterates_backward() {
        let preset = make_cc_cycle_preset(true);
        let mut state = PresetState::default();

        // First press at index 0: value=0, then index wraps to 3
        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 0]));

        // Second press at index 3: value=127, then index becomes 2
        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 127]));

        // Third press at index 2: value=64
        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 50, 64]));
    }

    #[test]
    fn cc_cycle_empty_values_no_output() {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press
            .push(Action::CcCycle {
                cc: 50,
                channel: 1,
                reverse: false,
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
                cycle_values: heapless::Vec::new(), // empty!
                listen_cc: None,
            })
            .ok();
        let preset = Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        };
        let mut state = PresetState::default();

        let r = process_button(&mut state, &preset, 0, ButtonEvent::Press);
        assert!(r.midi.is_empty());
    }

    // --- process_analog tests ---

    fn make_analog_preset(cc: u8, channel: u8, min: u8, max: u8) -> Preset {
        let mut analog: heapless::Vec<AnalogConfig, MAX_ANALOG> = heapless::Vec::new();
        analog
            .push(AnalogConfig {
                label: Label::try_from("Wah").unwrap(),
                cc,
                channel,
                min,
                max,
            })
            .ok();
        Preset {
            name: Label::new(),
            buttons: heapless::Vec::new(),
            encoders: heapless::Vec::new(),
            analog,
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
        }
    }

    #[test]
    fn analog_full_range_maps_to_max() {
        let preset = make_analog_preset(11, 1, 0, 127);
        let r = process_analog(&preset, 0, 3750, 0, 3750);
        assert_eq!(r.midi.len(), 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 11, 127]));
    }

    #[test]
    fn analog_min_maps_to_zero() {
        let preset = make_analog_preset(11, 1, 0, 127);
        let r = process_analog(&preset, 0, 0, 0, 3750);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 11, 0]));
    }

    #[test]
    fn analog_midpoint() {
        let preset = make_analog_preset(11, 1, 0, 127);
        let r = process_analog(&preset, 0, 1875, 0, 3750);
        // 1875/3750 * 127 = 63.5 → 63
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data[2] == 63));
    }

    #[test]
    fn analog_custom_range() {
        let preset = make_analog_preset(11, 1, 20, 100);
        let r = process_analog(&preset, 0, 3750, 0, 3750);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 11, 100]));

        let r = process_analog(&preset, 0, 0, 0, 3750);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 11, 20]));
    }

    #[test]
    fn analog_clamps_below_adc_min() {
        let preset = make_analog_preset(11, 1, 0, 127);
        // raw=100 but adc_min=200 — clamps to min
        let r = process_analog(&preset, 0, 100, 200, 3750);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data[2] == 0));
    }

    #[test]
    fn analog_clamps_above_adc_max() {
        let preset = make_analog_preset(11, 1, 0, 127);
        // raw=4000 but adc_max=3750 — clamps to max
        let r = process_analog(&preset, 0, 4000, 0, 3750);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data[2] == 127));
    }

    #[test]
    fn analog_emits_display_event() {
        let preset = make_analog_preset(11, 1, 0, 127);
        let r = process_analog(&preset, 0, 1875, 0, 3750);
        assert_eq!(r.display.len(), 1);
        match &r.display[0] {
            DisplayEvent::AnalogOverlay { side, label, value } => {
                assert_eq!(*side, DisplaySide::L);
                assert_eq!(label.as_str(), "Wah");
                assert_eq!(*value, 63);
            }
            _ => panic!("expected AnalogOverlay"),
        }
    }

    #[test]
    fn analog_invalid_index_returns_empty() {
        let preset = make_analog_preset(11, 1, 0, 127);
        let r = process_analog(&preset, 5, 1875, 0, 3750);
        assert!(r.midi.is_empty());
        assert!(r.display.is_empty());
    }

    // --- Trigger tests ---

    fn make_trigger_preset(triggers: heapless::Vec<Trigger, MAX_TRIGGERS>) -> Preset {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Toggle,
                on_press,
                on_release: heapless::Vec::new(),
                on_long_press: heapless::Vec::new(),
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();
        Preset {
            name: Label::new(),
            buttons,
            encoders: heapless::Vec::new(),
            analog: heapless::Vec::new(),
            defaults: Default::default(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers,
        }
    }

    #[test]
    fn trigger_cc_activates_button() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::Cc {
                    cc: 100,
                    channel: 1,
                    value_min: 64,
                    value_max: 127,
                },
                action: TriggerAction::Activate(0),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();

        let r = process_triggers(&mut state, &preset, 0xB0, 100, 127);
        assert!(state.button_active[0]);
        assert!(r.led_dirty);
    }

    #[test]
    fn trigger_cc_below_value_min_no_match() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::Cc {
                    cc: 100,
                    channel: 1,
                    value_min: 64,
                    value_max: 127,
                },
                action: TriggerAction::Activate(0),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();

        let r = process_triggers(&mut state, &preset, 0xB0, 100, 63);
        assert!(!state.button_active[0]);
        assert!(!r.led_dirty);
    }

    #[test]
    fn trigger_deactivate_button() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::Cc {
                    cc: 100,
                    channel: 1,
                    value_min: 0,
                    value_max: 63,
                },
                action: TriggerAction::Deactivate(0),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();
        state.button_active[0] = true;

        let r = process_triggers(&mut state, &preset, 0xB0, 100, 0);
        assert!(!state.button_active[0]);
        assert!(r.led_dirty);
    }

    #[test]
    fn trigger_program_change_selects_preset() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::ProgramChange {
                    program: 5,
                    channel: 1,
                },
                action: TriggerAction::PresetSelect(3),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();

        let r = process_triggers(&mut state, &preset, 0xC0, 5, 0);
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.system[0], SystemAction::PresetSelect(3));
    }

    #[test]
    fn trigger_note_on_executes_button() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::NoteOn {
                    note: 60,
                    channel: 1,
                },
                action: TriggerAction::Execute(0),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();

        let r = process_triggers(&mut state, &preset, 0x90, 60, 127);
        // Execute fires button 0's on_press: CC#80=127
        assert_eq!(r.midi.len(), 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(m) if m.data == [0xB0, 80, 127]));
    }

    #[test]
    fn trigger_note_on_velocity_zero_no_match() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::NoteOn {
                    note: 60,
                    channel: 1,
                },
                action: TriggerAction::Activate(0),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();

        // Note On with velocity 0 = Note Off, should not match
        let r = process_triggers(&mut state, &preset, 0x90, 60, 0);
        assert!(!state.button_active[0]);
        assert!(!r.led_dirty);
    }

    #[test]
    fn trigger_wrong_channel_no_match() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::Cc {
                    cc: 100,
                    channel: 2,
                    value_min: 0,
                    value_max: 127,
                },
                action: TriggerAction::Activate(0),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();

        // Send on channel 1 (0xB0), trigger expects channel 2
        let r = process_triggers(&mut state, &preset, 0xB0, 100, 127);
        assert!(!state.button_active[0]);
        assert!(!r.led_dirty);
    }

    #[test]
    fn trigger_multiple_triggers_all_fire() {
        let mut triggers: heapless::Vec<Trigger, MAX_TRIGGERS> = heapless::Vec::new();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::Cc {
                    cc: 100,
                    channel: 1,
                    value_min: 0,
                    value_max: 127,
                },
                action: TriggerAction::Activate(0),
            })
            .ok();
        triggers
            .push(Trigger {
                match_msg: TriggerMatch::Cc {
                    cc: 100,
                    channel: 1,
                    value_min: 64,
                    value_max: 127,
                },
                action: TriggerAction::PresetSelect(2),
            })
            .ok();
        let preset = make_trigger_preset(triggers);
        let mut state = PresetState::default();

        // CC#100 = 100 on ch1: matches both triggers
        let r = process_triggers(&mut state, &preset, 0xB0, 100, 100);
        assert!(state.button_active[0]);
        assert!(r.led_dirty);
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.system[0], SystemAction::PresetSelect(2));
    }
}
