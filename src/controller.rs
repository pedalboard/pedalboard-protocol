//! Unified controller: single entry point for all input processing.
//!
//! Owns all timing-sensitive state (long-press detection, encoder acceleration,
//! tap tempo) and delegates pure logic to the engine.
//!
//! Both firmware and simulator use the same `Controller` — the only difference
//! is where `now_ms` comes from (hardware monotonic vs std::Instant).

use crate::action::EncoderDirection;
use crate::config::{ButtonMode, Preset};
use crate::encoder_accel::EncoderAccel;
use crate::engine::{self, ActionStep, ButtonEvent, DisplayEvent, EngineResult, SystemAction};
use crate::long_press::{Edge, Gesture, LongPressDetector};
use crate::state::{PresetState, PresetStateStore};
use crate::tap_tempo::TapTempo;

const NUM_BUTTONS: usize = 6;
const NUM_ENCODERS: usize = 2;

/// Abstract input event. Hardware-agnostic — firmware maps GPIO edges to these,
/// the simulator maps keyboard/WebSocket events to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Button edge (index 0..5).
    ButtonEdge { index: u8, edge: Edge },
    /// Encoder detent (index 0..1).
    EncoderTurn { index: u8, clockwise: bool },
    /// Analog input (expression pedal). Raw ADC value.
    Analog {
        index: u8,
        raw: u16,
        min: u16,
        max: u16,
    },
}

/// Result of processing an input event through the Controller.
pub struct ControllerResult {
    /// MIDI actions to emit.
    pub midi: heapless::Vec<ActionStep, 8>,
    /// System-level actions (preset switch, set BPM).
    pub system: heapless::Vec<SystemAction, 2>,
    /// Display events (overlays, hints).
    pub display: heapless::Vec<DisplayEvent, 2>,
    /// Whether LED state changed and needs re-rendering.
    pub led_dirty: bool,
}

impl ControllerResult {
    fn new() -> Self {
        Self {
            midi: heapless::Vec::new(),
            system: heapless::Vec::new(),
            display: heapless::Vec::new(),
            led_dirty: false,
        }
    }

    fn merge_engine(&mut self, r: &EngineResult) {
        for step in &r.midi {
            self.midi.push(step.clone()).ok();
        }
        for s in &r.system {
            self.system.push(*s).ok();
        }
        for d in &r.display {
            self.display.push(d.clone()).ok();
        }
        if r.led_dirty {
            self.led_dirty = true;
        }
    }
}

/// Unified timing controller. Owns all stateful input processing.
pub struct Controller {
    state_store: PresetStateStore,
    long_press: [LongPressDetector; NUM_BUTTONS],
    encoder_accel: [EncoderAccel; NUM_ENCODERS],
    tap_tempo: TapTempo,
    /// Cached button active state (for momentary visual feedback during long-press).
    button_active: [bool; NUM_BUTTONS],
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl Controller {
    pub fn new() -> Self {
        Self {
            state_store: PresetStateStore::new(),
            long_press: core::array::from_fn(|_| LongPressDetector::new_fired()),
            encoder_accel: [EncoderAccel::new(), EncoderAccel::new()],
            tap_tempo: TapTempo::new(),
            button_active: [false; NUM_BUTTONS],
        }
    }

    /// Create with a restored state store (from EEPROM/persistence).
    pub fn with_state(store: PresetStateStore) -> Self {
        let state = store.current().clone();
        Self {
            state_store: store,
            long_press: core::array::from_fn(|_| LongPressDetector::new_fired()),
            encoder_accel: [EncoderAccel::new(), EncoderAccel::new()],
            tap_tempo: TapTempo::new(),
            button_active: state.button_active,
        }
    }

    /// Process a single input event with the current timestamp.
    /// Returns MIDI, system actions, display events, and LED dirty flag.
    pub fn process(&mut self, event: InputEvent, now_ms: u32, preset: &Preset) -> ControllerResult {
        match event {
            InputEvent::ButtonEdge { index, edge } => {
                self.process_button(index as usize, edge, now_ms, preset)
            }
            InputEvent::EncoderTurn { index, clockwise } => {
                self.process_encoder(index as usize, clockwise, now_ms, preset)
            }
            InputEvent::Analog {
                index,
                raw,
                min,
                max,
            } => self.process_analog(index as usize, raw, min, max, preset),
        }
    }

    /// Poll with no event — needed for long-press detection while button is held.
    /// Call this periodically (e.g., every 1ms) when a button is active.
    pub fn tick(&mut self, now_ms: u32, preset: &Preset) -> ControllerResult {
        let mut result = ControllerResult::new();
        for i in 0..NUM_BUTTONS {
            if self.long_press[i].is_active() && !self.long_press[i].has_fired() {
                let has_long_press = preset
                    .buttons
                    .get(i)
                    .map(|b| !b.on_long_press.is_empty())
                    .unwrap_or(false);
                if has_long_press {
                    if let Some(gesture) = self.long_press[i].update(None, now_ms) {
                        self.handle_gesture(i, gesture, preset, &mut result);
                    }
                }
            }
        }
        result
    }

    /// Returns true if any button is currently held.
    pub fn any_active(&self) -> bool {
        self.long_press.iter().any(|lp| lp.is_active())
    }

    /// Returns the current button active state (toggle ON / momentary held).
    pub fn button_active(&self) -> &[bool; NUM_BUTTONS] {
        &self.button_active
    }

    /// Get the current preset state.
    pub fn state(&self) -> &PresetState {
        self.state_store.current()
    }

    /// Apply external state changes (e.g., from MIDI triggers).
    pub fn save_working(&mut self, state: &PresetState) {
        self.button_active = state.button_active;
        self.state_store.save_working(state);
    }

    /// Get the active preset index.
    pub fn active_preset(&self) -> u8 {
        self.state_store.active_index()
    }

    /// Get the encoder values.
    pub fn encoder_values(&self) -> [u8; NUM_ENCODERS] {
        self.state_store.current().encoder_values
    }

    /// Get tap tempo.
    pub fn tap_tempo(&mut self) -> &mut TapTempo {
        &mut self.tap_tempo
    }

    /// Switch to a new preset. Saves current state, loads new.
    /// Returns recall MIDI messages (toggle/encoder state).
    pub fn switch_preset(
        &mut self,
        new_preset_idx: u8,
        new_preset: &Preset,
    ) -> heapless::Vec<ActionStep, 16> {
        let mut working = self.working_state();
        let recall = self
            .state_store
            .switch(new_preset_idx, &mut working, new_preset);
        self.apply_state(&working);
        // Reset long-press to suppress stale releases
        self.long_press = core::array::from_fn(|_| LongPressDetector::new_fired());
        self.encoder_accel = [EncoderAccel::new(), EncoderAccel::new()];

        let mut result = heapless::Vec::new();
        for msg in &recall {
            result.push(ActionStep::Send(msg.clone())).ok();
        }
        result
    }

    /// Serialize current state for EEPROM persistence.
    pub fn eeprom_state(&self) -> heapless::Vec<u8, 128> {
        let mut buf = [0u8; 128];
        let mut store_copy = self.state_store.clone();
        let working = self.working_state();
        store_copy.save_working(&working);
        store_copy.to_eeprom(&mut buf);
        heapless::Vec::from_slice(&buf).unwrap_or_default()
    }

    /// Milliseconds the given button has been held, or 0 if not active.
    pub fn held_ms(&self, button_index: usize, now_ms: u32) -> u32 {
        if button_index < NUM_BUTTONS {
            self.long_press[button_index].held_ms(now_ms)
        } else {
            0
        }
    }

    // --- Private ---

    fn process_button(
        &mut self,
        index: usize,
        edge: Edge,
        now_ms: u32,
        preset: &Preset,
    ) -> ControllerResult {
        let mut result = ControllerResult::new();

        if index >= NUM_BUTTONS {
            return result;
        }

        let has_long_press = preset
            .buttons
            .get(index)
            .map(|b| !b.on_long_press.is_empty())
            .unwrap_or(false);

        let mode = preset
            .buttons
            .get(index)
            .map(|b| &b.mode)
            .unwrap_or(&ButtonMode::Momentary);

        if has_long_press {
            // Momentary visual feedback while held
            if matches!(mode, ButtonMode::Momentary) {
                match edge {
                    Edge::Activate => {
                        self.button_active[index] = true;
                        result.led_dirty = true;
                    }
                    Edge::Deactivate => {
                        self.button_active[index] = false;
                        result.led_dirty = true;
                    }
                }
            }

            // Emit display hint on press
            if edge == Edge::Activate {
                if let Some(btn) = preset.buttons.get(index) {
                    let hint_action = btn.on_long_press.iter().find_map(|a| {
                        use crate::config::Action;
                        match a {
                            Action::PresetNext => Some(SystemAction::PresetNext),
                            Action::PresetPrev => Some(SystemAction::PresetPrev),
                            _ => None,
                        }
                    });
                    if let Some(action) = hint_action {
                        result
                            .display
                            .push(DisplayEvent::LongPressHint { action })
                            .ok();
                    }
                }
            }

            // Long-press detection
            if let Some(gesture) = self.long_press[index].update(Some(edge), now_ms) {
                self.handle_gesture(index, gesture, preset, &mut result);
            }
        } else {
            // No long-press configured: immediate dispatch
            match edge {
                Edge::Activate => {
                    let mut state = self.working_state();
                    let r = engine::process_button(&mut state, preset, index, ButtonEvent::Press);
                    self.apply_state(&state);
                    result.merge_engine(&r);
                }
                Edge::Deactivate => {
                    let mut state = self.working_state();
                    let r = engine::process_button(&mut state, preset, index, ButtonEvent::Release);
                    self.apply_state(&state);
                    result.merge_engine(&r);
                }
            }
        }

        result
    }

    fn handle_gesture(
        &mut self,
        index: usize,
        gesture: Gesture,
        preset: &Preset,
        result: &mut ControllerResult,
    ) {
        let mode = preset
            .buttons
            .get(index)
            .map(|b| &b.mode)
            .unwrap_or(&ButtonMode::Momentary);

        match gesture {
            Gesture::ShortPress => {
                result.display.push(DisplayEvent::LongPressCancel).ok();
                let mut state = self.working_state();
                let r = engine::process_button(&mut state, preset, index, ButtonEvent::Press);
                self.apply_state(&state);
                result.merge_engine(&r);

                // For momentary or buttons with on_release: also fire release
                if let Some(btn) = preset.buttons.get(index) {
                    if mode == &ButtonMode::Momentary || !btn.on_release.is_empty() {
                        let mut state2 = self.working_state();
                        let r2 = engine::process_button(
                            &mut state2,
                            preset,
                            index,
                            ButtonEvent::Release,
                        );
                        self.apply_state(&state2);
                        result.merge_engine(&r2);
                    }
                }
            }
            Gesture::LongPress => {
                let mut state = self.working_state();
                let r = engine::process_button(&mut state, preset, index, ButtonEvent::LongPress);
                self.apply_state(&state);
                result.merge_engine(&r);
            }
        }
    }

    fn process_encoder(
        &mut self,
        index: usize,
        clockwise: bool,
        now_ms: u32,
        preset: &Preset,
    ) -> ControllerResult {
        let mut result = ControllerResult::new();
        if index >= NUM_ENCODERS {
            return result;
        }

        let steps = self.encoder_accel[index].steps(now_ms);
        let direction = if clockwise {
            EncoderDirection::Clockwise
        } else {
            EncoderDirection::CounterClockwise
        };

        let mut state = self.working_state();
        let r = engine::process_encoder(&mut state, preset, index, direction, steps);
        self.apply_state(&state);
        result.merge_engine(&r);
        result
    }

    fn process_analog(
        &mut self,
        index: usize,
        raw: u16,
        min: u16,
        max: u16,
        preset: &Preset,
    ) -> ControllerResult {
        let mut result = ControllerResult::new();
        let r = engine::process_analog(preset, index, raw, min, max);
        result.merge_engine(&r);
        result
    }

    fn working_state(&self) -> PresetState {
        let current = self.state_store.current();
        PresetState {
            button_active: self.button_active,
            cycle_index: current.cycle_index,
            encoder_values: current.encoder_values,
        }
    }

    fn apply_state(&mut self, state: &PresetState) {
        self.button_active = state.button_active;
        self.state_store.save_working(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn make_preset() -> Preset {
        let mut buttons: heapless::Vec<ButtonConfig, MAX_BUTTONS> = heapless::Vec::new();

        // Button 0: simple toggle CC
        let mut on_press: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press.push(Action::cc(0, 127, 1).unwrap()).ok();
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

        // Button 1: momentary with long_press → PresetNext
        let mut on_press_b: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_press_b.push(Action::cc(10, 127, 1).unwrap()).ok();
        let mut on_lp: heapless::Vec<Action, MAX_ACTIONS> = heapless::Vec::new();
        on_lp.push(Action::PresetNext).ok();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press: on_press_b,
                on_release: heapless::Vec::new(),
                on_long_press: on_lp,
                cycle_values: heapless::Vec::new(),
                listen_cc: None,
            })
            .ok();

        // Encoders
        let mut encoders: heapless::Vec<EncoderConfig, MAX_ENCODERS> = heapless::Vec::new();
        encoders
            .push(EncoderConfig {
                label: Label::new(),
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
                label: Label::new(),
                action: EncoderAction::Cc {
                    cc: 11,
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
            analog: heapless::Vec::new(),
            on_enter: heapless::Vec::new(),
            on_exit: heapless::Vec::new(),
            triggers: heapless::Vec::new(),
            defaults: InitialState::default(),
        }
    }

    #[test]
    fn simple_button_press_generates_midi() {
        let preset = make_preset();
        let mut ctrl = Controller::new();
        let r = ctrl.process(
            InputEvent::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &preset,
        );
        assert_eq!(r.midi.len(), 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(msg) if msg.data[..2] == [0xB0, 0]));
    }

    #[test]
    fn short_press_with_long_press_configured() {
        let preset = make_preset();
        let mut ctrl = Controller::new();
        // Press button 1
        ctrl.process(
            InputEvent::ButtonEdge {
                index: 1,
                edge: Edge::Activate,
            },
            0,
            &preset,
        );
        // Hold for 100ms
        ctrl.tick(100, &preset);
        // Release at 200ms — should fire short press
        let r = ctrl.process(
            InputEvent::ButtonEdge {
                index: 1,
                edge: Edge::Deactivate,
            },
            200,
            &preset,
        );
        assert_eq!(r.midi.len(), 1);
        assert!(matches!(&r.midi[0], ActionStep::Send(msg) if msg.data[..2] == [0xB0, 10]));
    }

    #[test]
    fn long_press_fires_system_action() {
        let preset = make_preset();
        let mut ctrl = Controller::new();
        // Press button 1
        ctrl.process(
            InputEvent::ButtonEdge {
                index: 1,
                edge: Edge::Activate,
            },
            0,
            &preset,
        );
        // Tick past threshold
        let r = ctrl.tick(500, &preset);
        assert!(!r.system.is_empty());
        assert_eq!(r.system[0], SystemAction::PresetNext);
    }

    #[test]
    fn long_press_suppresses_short_press() {
        let preset = make_preset();
        let mut ctrl = Controller::new();
        ctrl.process(
            InputEvent::ButtonEdge {
                index: 1,
                edge: Edge::Activate,
            },
            0,
            &preset,
        );
        ctrl.tick(500, &preset);
        // Release after long press
        let r = ctrl.process(
            InputEvent::ButtonEdge {
                index: 1,
                edge: Edge::Deactivate,
            },
            600,
            &preset,
        );
        assert!(r.midi.is_empty());
        assert!(r.system.is_empty());
    }

    #[test]
    fn encoder_turn_generates_cc() {
        let preset = make_preset();
        let mut ctrl = Controller::new();
        // Set initial encoder value
        ctrl.state_store.save_working(&PresetState {
            button_active: [false; 6],
            cycle_index: [0; 6],
            encoder_values: [64, 64],
        });
        let r = ctrl.process(
            InputEvent::EncoderTurn {
                index: 0,
                clockwise: true,
            },
            100,
            &preset,
        );
        assert_eq!(r.midi.len(), 1);
        assert!(
            matches!(&r.midi[0], ActionStep::Send(msg) if msg.data[0] == 0xB0 && msg.data[1] == 7)
        );
    }

    #[test]
    fn analog_generates_cc() {
        let mut preset = make_preset();
        preset
            .analog
            .push(AnalogConfig {
                label: Label::new(),
                cc: 11,
                channel: 1,
                min: 0,
                max: 127,
            })
            .ok();

        let mut ctrl = Controller::new();
        let r = ctrl.process(
            InputEvent::Analog {
                index: 0,
                raw: 2000,
                min: 0,
                max: 3750,
            },
            0,
            &preset,
        );
        assert_eq!(r.midi.len(), 1);
    }

    #[test]
    fn any_active_tracks_held_button() {
        let preset = make_preset();
        let mut ctrl = Controller::new();
        assert!(!ctrl.any_active());
        ctrl.process(
            InputEvent::ButtonEdge {
                index: 1,
                edge: Edge::Activate,
            },
            0,
            &preset,
        );
        assert!(ctrl.any_active());
        ctrl.process(
            InputEvent::ButtonEdge {
                index: 1,
                edge: Edge::Deactivate,
            },
            100,
            &preset,
        );
        assert!(!ctrl.any_active());
    }
}
