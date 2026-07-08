//! Unified controller: single entry point for all input processing.
//!
//! Owns all timing-sensitive state (long-press detection, encoder acceleration,
//! tap tempo, preset state) and delegates pure logic to the engine.
//!
//! The Controller handles system actions (preset switching) internally,
//! including on_enter/on_exit actions and recall MIDI. Callers only need to
//! send the resulting MIDI output — no orchestration required.
//!
//! Both firmware and simulator use the same `Controller` — the only difference
//! is where `now_ms` comes from (hardware monotonic vs std::Instant).

use crate::action::{action_to_midi, EncoderDirection};
use crate::config::{Action, ButtonMode, Config, Preset};
use crate::encoder_accel::EncoderAccel;
use crate::engine::{
    self, process_triggers, ActionStep, ButtonEvent, DisplayEvent, EngineResult, SystemAction,
};
use crate::long_press::{Edge, Gesture, LongPressDetector};
use crate::state::{PresetState, PresetStateStore};
use crate::tap_tempo::TapTempo;

const NUM_BUTTONS: usize = 6;
const NUM_ENCODERS: usize = 2;

/// Abstract input event. Hardware-agnostic — firmware maps GPIO edges to these,
/// the simulator maps keyboard/WebSocket events to these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Button edge (index 0..5).
    ButtonEdge { index: u8, edge: Edge },
    /// Encoder detent (index 0..1).
    EncoderTurn { index: u8, clockwise: bool },
    /// Analog input (expression pedal). Raw ADC value.
    /// Calibration (min/max) is read from Config.global.
    Analog { index: u8, raw: u16 },
    /// Incoming MIDI (for trigger processing). Up to 3 bytes.
    IncomingMidi { data: [u8; 3], len: u8 },
    /// Periodic tick — drives long-press detection. Send every 1-10ms.
    /// No-op if no buttons are held.
    Tick,
}

/// Result of processing an input event through the Controller.
/// Contains everything the caller needs to emit — no further logic required.
pub struct Output {
    /// MIDI actions to emit (includes on_enter/on_exit/recall on preset switch).
    pub midi: heapless::Vec<ActionStep, 32>,
    /// Display events (overlays, hints).
    pub display: heapless::Vec<DisplayEvent, 2>,
    /// Whether LED state changed and needs re-rendering.
    pub leds_changed: bool,
    /// Whether a preset switch occurred (caller may need to update display).
    pub preset_changed: bool,
    /// BPM computed from tap tempo (if any).
    pub bpm: Option<u16>,
    /// Internal: pending system actions to be handled by the controller.
    pending_system: heapless::Vec<SystemAction, 2>,
}

impl Output {
    fn new() -> Self {
        Self {
            midi: heapless::Vec::new(),
            display: heapless::Vec::new(),
            leds_changed: false,
            preset_changed: false,
            bpm: None,
            pending_system: heapless::Vec::new(),
        }
    }
}

/// Unified timing controller. Owns all stateful input processing.
///
/// # Usage
///
/// ```ignore
/// let mut ctrl = Controller::new();
/// // On each input poll:
/// let result = ctrl.process(event, now_ms, &config);
/// // Send result.midi via MIDI output
/// // Update display from result.display
/// // Re-render LEDs if result.leds_changed
/// ```
pub struct Controller {
    state_store: PresetStateStore,
    long_press: [LongPressDetector; NUM_BUTTONS],
    encoder_accel: [EncoderAccel; NUM_ENCODERS],
    tap_tempo: TapTempo,
    button_active: [bool; NUM_BUTTONS],
    active_preset: u8,
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
            active_preset: 0,
        }
    }

    /// Create with a restored state store (from EEPROM/persistence).
    pub fn with_state(store: PresetStateStore) -> Self {
        let state = store.current().clone();
        let active = store.active_index();
        Self {
            state_store: store,
            long_press: core::array::from_fn(|_| LongPressDetector::new_fired()),
            encoder_accel: [EncoderAccel::new(), EncoderAccel::new()],
            tap_tempo: TapTempo::new(),
            button_active: state.button_active,
            active_preset: active,
        }
    }

    /// Process a single input event. Returns all MIDI output, display events,
    /// and flags. System actions (preset switch, tap tempo) are handled internally.
    pub fn process(&mut self, event: Event, now_ms: u32, config: &Config) -> Output {
        let mut result = match event {
            Event::ButtonEdge { index, edge } => {
                self.process_button(index as usize, edge, now_ms, config)
            }
            Event::EncoderTurn { index, clockwise } => {
                self.process_encoder(index as usize, clockwise, now_ms, config)
            }
            Event::Analog { index, raw } => self.process_analog(index as usize, raw, config),
            Event::IncomingMidi { data, len } => {
                self.do_process_incoming_midi(&data[..len as usize], config)
            }
            Event::Tick => self.do_tick(now_ms, config),
        };

        // Handle system actions produced by event processing
        self.handle_system_actions(&mut result, now_ms, config);

        result
    }

    fn do_tick(&mut self, now_ms: u32, config: &Config) -> Output {
        let mut result = Output::new();

        if !self.button_held() {
            return result;
        }

        let preset = match config.presets.get(self.active_preset as usize) {
            Some(p) => p,
            None => return result,
        };

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

    fn do_process_incoming_midi(&mut self, raw: &[u8], config: &Config) -> Output {
        let mut result = Output::new();

        let preset = match config.presets.get(self.active_preset as usize) {
            Some(p) => p,
            None => return result,
        };

        if raw.len() >= 2 && !preset.triggers.is_empty() {
            let mut state = self.working_state();
            let data2 = if raw.len() >= 3 { raw[2] } else { 0 };
            let trigger_result = process_triggers(&mut state, preset, raw[0], raw[1], data2);
            self.apply_state(&state);

            for step in &trigger_result.midi {
                result.midi.push(step.clone()).ok();
            }
            if trigger_result.led_dirty {
                result.leds_changed = true;
            }

            // Handle system actions from triggers (preset switch)
            for s in &trigger_result.system {
                self.execute_system_action(*s, &mut result, 0, config);
            }
        }

        result
    }

    /// Returns true if any button is currently held.
    pub fn button_held(&self) -> bool {
        self.long_press.iter().any(|lp| lp.is_active())
    }

    /// Returns the current button active state (toggle ON / momentary held).
    pub fn button_states(&self) -> &[bool; NUM_BUTTONS] {
        &self.button_active
    }

    /// Get the active preset index.
    pub fn active_preset(&self) -> u8 {
        self.active_preset
    }

    /// Get the encoder values.
    pub fn encoder_values(&self) -> [u8; NUM_ENCODERS] {
        self.state_store.current().encoder_values
    }

    /// Serialize current state for EEPROM persistence.
    pub fn save_state(&self) -> heapless::Vec<u8, 128> {
        let mut buf = [0u8; 128];
        let mut store_copy = self.state_store.clone();
        let working = self.working_state();
        store_copy.save_working(&working);
        store_copy.to_eeprom(&mut buf);
        heapless::Vec::from_slice(&buf).unwrap_or_default()
    }

    /// Manually switch to a preset (e.g., on boot or from external command).
    /// Returns on_enter + recall MIDI in the result.
    pub fn select_preset(&mut self, preset_idx: u8, config: &Config) -> Output {
        let mut result = Output::new();
        self.do_switch_preset(preset_idx, &mut result, config);
        result
    }

    /// Set encoder value (for initial state setup from config defaults).
    pub fn set_encoder_value(&mut self, index: usize, value: u8) {
        let mut state = self.state_store.current().clone();
        if index < NUM_ENCODERS {
            state.encoder_values[index] = value;
        }
        self.state_store.save_working(&state);
    }

    // --- Private ---

    fn current_preset<'a>(&self, config: &'a Config) -> Option<&'a Preset> {
        config.presets.get(self.active_preset as usize)
    }

    fn handle_system_actions(&mut self, result: &mut Output, now_ms: u32, config: &Config) {
        let actions: heapless::Vec<SystemAction, 2> = result.pending_system.clone();
        result.pending_system.clear();
        for action in &actions {
            self.execute_system_action(*action, result, now_ms, config);
        }
    }

    fn process_button(&mut self, index: usize, edge: Edge, now_ms: u32, config: &Config) -> Output {
        let mut result = Output::new();

        let preset = match self.current_preset(config) {
            Some(p) => p,
            None => return result,
        };

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
            if matches!(mode, ButtonMode::Momentary) {
                match edge {
                    Edge::Activate => {
                        self.button_active[index] = true;
                        result.leds_changed = true;
                    }
                    Edge::Deactivate => {
                        self.button_active[index] = false;
                        result.leds_changed = true;
                    }
                }
            }

            if edge == Edge::Activate {
                if let Some(btn) = preset.buttons.get(index) {
                    let hint_action = btn.on_long_press.iter().find_map(|a| match a {
                        Action::PresetNext => Some(SystemAction::PresetNext),
                        Action::PresetPrev => Some(SystemAction::PresetPrev),
                        _ => None,
                    });
                    if let Some(action) = hint_action {
                        result
                            .display
                            .push(DisplayEvent::LongPressHint { action })
                            .ok();
                    }
                }
            }

            if let Some(gesture) = self.long_press[index].update(Some(edge), now_ms) {
                self.handle_gesture(index, gesture, preset, &mut result);
            }
        } else {
            match edge {
                Edge::Activate => {
                    let mut state = self.working_state();
                    let r = engine::process_button(&mut state, preset, index, ButtonEvent::Press);
                    self.apply_state(&state);
                    self.merge_engine_result(&r, &mut result);
                }
                Edge::Deactivate => {
                    let mut state = self.working_state();
                    let r = engine::process_button(&mut state, preset, index, ButtonEvent::Release);
                    self.apply_state(&state);
                    self.merge_engine_result(&r, &mut result);
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
        result: &mut Output,
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
                self.merge_engine_result(&r, result);

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
                        self.merge_engine_result(&r2, result);
                    }
                }
            }
            Gesture::LongPress => {
                let mut state = self.working_state();
                let r = engine::process_button(&mut state, preset, index, ButtonEvent::LongPress);
                self.apply_state(&state);
                self.merge_engine_result(&r, result);
            }
        }
    }

    fn process_encoder(
        &mut self,
        index: usize,
        clockwise: bool,
        now_ms: u32,
        config: &Config,
    ) -> Output {
        let mut result = Output::new();
        let preset = match self.current_preset(config) {
            Some(p) => p,
            None => return result,
        };

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
        self.merge_engine_result(&r, &mut result);
        result
    }

    fn process_analog(&mut self, index: usize, raw: u16, config: &Config) -> Output {
        let mut result = Output::new();
        let preset = match self.current_preset(config) {
            Some(p) => p,
            None => return result,
        };
        let (min, max) = match index {
            0 => (config.global.exp1_min, config.global.exp1_max),
            1 => (config.global.exp2_min, config.global.exp2_max),
            _ => return result,
        };
        let r = engine::process_analog(preset, index, raw, min, max);
        self.merge_engine_result(&r, &mut result);
        result
    }

    /// Merge engine result into controller result, intercepting system actions.
    fn merge_engine_result(&mut self, r: &EngineResult, result: &mut Output) {
        for step in &r.midi {
            result.midi.push(step.clone()).ok();
        }
        for d in &r.display {
            result.display.push(d.clone()).ok();
        }
        if r.led_dirty {
            result.leds_changed = true;
        }
        // System actions are collected — will be handled by the caller (process/tick)
        // via handle_system_actions after the event processing completes.
        // We store them temporarily in a hidden field... but Output doesn't
        // have system anymore. Let's use a simpler approach:
        // We handle them inline by storing in a temp vec on Output.
        for s in &r.system {
            result.pending_system.push(*s).ok();
        }
    }

    fn execute_system_action(
        &mut self,
        action: SystemAction,
        result: &mut Output,
        now_ms: u32,
        config: &Config,
    ) {
        let num_presets = config.presets.iter().filter(|p| !p.name.is_empty()).count() as u8;

        match action {
            SystemAction::PresetNext => {
                if num_presets > 0 {
                    let next = (self.active_preset + 1) % num_presets;
                    self.do_switch_preset(next, result, config);
                }
            }
            SystemAction::PresetPrev => {
                if num_presets > 0 {
                    let prev = if self.active_preset == 0 {
                        num_presets - 1
                    } else {
                        self.active_preset - 1
                    };
                    self.do_switch_preset(prev, result, config);
                }
            }
            SystemAction::PresetSelect(idx) => {
                if (idx as usize) < config.presets.len() {
                    self.do_switch_preset(idx, result, config);
                }
            }
            SystemAction::SetBpm(bpm) => {
                result.bpm = Some(bpm);
                result.display.push(DisplayEvent::BpmOverlay { bpm }).ok();
            }
            SystemAction::TapTempo => {
                result.bpm = self.tap_tempo.tap(now_ms);
                if let Some(bpm) = result.bpm {
                    result.display.push(DisplayEvent::BpmOverlay { bpm }).ok();
                }
            }
        }
    }

    fn do_switch_preset(&mut self, new_idx: u8, result: &mut Output, config: &Config) {
        let old_preset = config.presets.get(self.active_preset as usize);
        let new_preset = config.presets.get(new_idx as usize);

        // Fire on_exit for old preset
        if let Some(old) = old_preset {
            for action in &old.on_exit {
                match action {
                    Action::Delay(ms) => {
                        result.midi.push(ActionStep::Delay(*ms)).ok();
                    }
                    _ => {
                        if let Some(msg) = action_to_midi(action) {
                            result.midi.push(ActionStep::Send(msg)).ok();
                        }
                    }
                }
            }
        }

        // Switch state
        if let Some(new_p) = new_preset {
            let mut working = self.working_state();
            let recall = self.state_store.switch(new_idx, &mut working, new_p);
            self.apply_state(&working);
            self.long_press = core::array::from_fn(|_| LongPressDetector::new_fired());
            self.encoder_accel = [EncoderAccel::new(), EncoderAccel::new()];
            self.active_preset = new_idx;

            // Fire on_enter for new preset
            for action in &new_p.on_enter {
                match action {
                    Action::Delay(ms) => {
                        result.midi.push(ActionStep::Delay(*ms)).ok();
                    }
                    _ => {
                        if let Some(msg) = action_to_midi(action) {
                            result.midi.push(ActionStep::Send(msg)).ok();
                        }
                    }
                }
            }

            // Recall MIDI (state sync)
            for msg in &recall {
                result.midi.push(ActionStep::Send(msg.clone())).ok();
            }
        }

        result.preset_changed = true;
        result.leds_changed = true;
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
