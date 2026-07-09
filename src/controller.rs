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
use crate::config::{Action, ButtonMode, Config, Preset, MAX_BUTTONS, MAX_ENCODERS};
use crate::encoder_accel::EncoderAccel;
use crate::engine::{
    self, process_triggers, ActionStep, ButtonEvent, DisplayEvent, EngineResult, SystemAction,
};
use crate::long_press::{Edge, Gesture, LongPressDetector};
use crate::routing::MidiPort;
use crate::state::{PresetState, PresetStateStore};
use crate::tap_tempo::TapTempo;

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
    /// Incoming MIDI message from any port.
    Midi {
        data: [u8; 8],
        len: u8,
        source: MidiPort,
    },
    /// Periodic tick — drives long-press detection. Send every 1-10ms.
    /// No-op if no buttons are held.
    Tick,
}

/// Result of processing an input event through the Controller.
/// Contains everything the caller needs to emit — no further logic required.
pub struct Output {
    /// MIDI actions to emit (includes on_enter/on_exit/recall on preset switch).
    pub midi: heapless::Vec<ActionStep, 32>,
    /// Routed MIDI messages with destination port(s).
    /// Includes thru-forwarded messages and future controller-generated output.
    pub midi_out: heapless::Vec<crate::routing::MidiOut, 16>,
    /// Display events (overlays, hints).
    pub display: heapless::Vec<DisplayEvent, 2>,
    /// Whether LED state changed and needs re-rendering.
    pub leds_changed: bool,
    /// Whether a preset switch occurred (caller may need to update display).
    pub preset_changed: bool,
    /// BPM computed from tap tempo (if any).
    pub bpm: Option<u16>,
    /// Mon indicator LED: flashes on MIDI activity.
    /// Green = MIDI output generated, Blue = incoming MIDI processed.
    pub mon_led: Option<crate::led::Rgb>,
    /// Mode indicator LED: color represents the active preset/bank.
    /// Set on preset change; None means no change.
    pub mode_led: Option<crate::led::Rgb>,
    /// Reactive LED updates from incoming MIDI (CC → ring heatmap/trigger).
    pub reactive_led: Option<engine::ReactiveResult>,
    /// Internal: pending system actions to be handled by the controller.
    pending_system: heapless::Vec<SystemAction, 2>,
}

impl Output {
    fn new() -> Self {
        Self {
            midi: heapless::Vec::new(),
            midi_out: heapless::Vec::new(),
            display: heapless::Vec::new(),
            leds_changed: false,
            preset_changed: false,
            bpm: None,
            mon_led: None,
            mode_led: None,
            reactive_led: None,
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
pub struct Controller<const B: usize = MAX_BUTTONS, const E: usize = MAX_ENCODERS> {
    state_store: PresetStateStore<B, E>,
    long_press: [LongPressDetector; B],
    encoder_accel: [EncoderAccel; E],
    tap_tempo: TapTempo,
    button_active: [bool; B],
    active_preset: u8,
}

/// Type alias for the default controller configuration (6 buttons, 2 encoders).
pub type DefaultController = Controller<MAX_BUTTONS, MAX_ENCODERS>;

impl<const B: usize, const E: usize> Default for Controller<B, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const B: usize, const E: usize> Controller<B, E> {
    pub fn new() -> Self {
        Self {
            state_store: PresetStateStore::new(),
            long_press: core::array::from_fn(|_| LongPressDetector::new_fired()),
            encoder_accel: core::array::from_fn(|_| EncoderAccel::new()),
            tap_tempo: TapTempo::new(),
            button_active: [false; B],
            active_preset: 0,
        }
    }

    /// Create with a restored state store (from EEPROM/persistence).
    pub fn with_state(store: PresetStateStore<B, E>) -> Self {
        let state = store.current().clone();
        let active = store.active_index();
        Self {
            state_store: store,
            long_press: core::array::from_fn(|_| LongPressDetector::new_fired()),
            encoder_accel: core::array::from_fn(|_| EncoderAccel::new()),
            tap_tempo: TapTempo::new(),
            button_active: state.button_active,
            active_preset: active,
        }
    }

    /// Process a single input event. Returns all MIDI output, display events,
    /// and flags. System actions (preset switch, tap tempo) are handled internally.
    pub fn process<const A: usize>(
        &mut self,
        event: Event,
        now_ms: u32,
        config: &Config<B, E, A>,
    ) -> Output {
        let mut result = match event {
            Event::ButtonEdge { index, edge } => {
                self.process_button(index as usize, edge, now_ms, config)
            }
            Event::EncoderTurn { index, clockwise } => {
                self.process_encoder(index as usize, clockwise, now_ms, config)
            }
            Event::Analog { index, raw } => self.process_analog(index as usize, raw, config),
            Event::Midi { data, len, source } => {
                self.do_process_incoming_midi(&data[..len as usize], source, config)
            }
            Event::Tick => self.do_tick(now_ms, config),
        };

        // Handle system actions produced by event processing
        self.handle_system_actions(&mut result, now_ms, config);

        // Indicator LEDs
        if !result.midi.is_empty() {
            // Green flash: MIDI output generated
            result.mon_led = Some(crate::led::Rgb::new(0, 255, 0));
        } else if matches!(event, Event::Midi { .. }) {
            // Blue flash: incoming MIDI processed (even if no output)
            result.mon_led = Some(crate::led::Rgb::new(0, 0, 255));
        }

        result
    }

    fn do_tick<const A: usize>(&mut self, now_ms: u32, config: &Config<B, E, A>) -> Output {
        let mut result = Output::new();

        if !self.button_held() {
            return result;
        }

        let preset = match config.presets.get(self.active_preset as usize) {
            Some(p) => p,
            None => return result,
        };

        for i in 0..B {
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

    fn do_process_incoming_midi<const A: usize>(
        &mut self,
        raw: &[u8],
        source: MidiPort,
        config: &Config<B, E, A>,
    ) -> Output {
        let mut result = Output::new();

        // Thru routing: forward incoming MIDI to configured destination ports
        let thru_dest = self.thru_destination(source, config);
        if !thru_dest.is_empty() && !raw.is_empty() {
            result
                .midi_out
                .push(crate::routing::MidiOut::new(raw, thru_dest))
                .ok();
        }

        let preset = match config.presets.get(self.active_preset as usize) {
            Some(p) => p,
            None => return result,
        };

        // Reactive LEDs: check incoming CC against preset's listen_cc bindings
        if raw.len() >= 3 && (raw[0] & 0xF0) == 0xB0 {
            let channel = (raw[0] & 0x0F) + 1;
            result.reactive_led = engine::process_incoming_cc(preset, channel, raw[1], raw[2]);
        }

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

    /// Compute thru destination ports for a given source port based on global config.
    fn thru_destination<const A: usize>(
        &self,
        source: MidiPort,
        config: &Config<B, E, A>,
    ) -> MidiPort {
        let mut dest = MidiPort::empty();
        if source.contains(MidiPort::DIN) && config.global.din_to_usb_thru {
            dest |= MidiPort::USB;
        }
        if source.contains(MidiPort::USB) {
            if config.global.usb_to_din_thru {
                dest |= MidiPort::DIN;
            }
            if config.global.usb_to_usb_thru {
                dest |= MidiPort::USB;
            }
        }
        dest
    }

    /// Returns true if any button is currently held.
    pub fn button_held(&self) -> bool {
        self.long_press.iter().any(|lp| lp.is_active())
    }

    /// Returns the current button active state (toggle ON / momentary held).
    pub fn button_states(&self) -> &[bool; B] {
        &self.button_active
    }

    /// Get the active preset index.
    pub fn active_preset(&self) -> u8 {
        self.active_preset
    }

    /// Get the encoder values.
    pub fn encoder_values(&self) -> [u8; E] {
        self.state_store.current().encoder_values
    }

    /// Get a snapshot of the state store (with current working state saved).
    /// Use this for persistence — the caller decides the serialization format.
    pub fn snapshot_store(&self) -> PresetStateStore<B, E> {
        let mut store = self.state_store.clone();
        let working = self.working_state();
        store.save_working(&working);
        store
    }

    /// Manually switch to a preset (e.g., on boot or from external command).
    /// Returns on_enter + recall MIDI in the result.
    pub fn select_preset<const A: usize>(
        &mut self,
        preset_idx: u8,
        config: &Config<B, E, A>,
    ) -> Output {
        let mut result = Output::new();
        self.do_switch_preset(preset_idx, &mut result, config);
        result
    }

    /// Set encoder value (for initial state setup from config defaults).
    pub fn set_encoder_value(&mut self, index: usize, value: u8) {
        let mut state = self.state_store.current().clone();
        if index < E {
            state.encoder_values[index] = value;
        }
        self.state_store.save_working(&state);
    }

    // --- Private ---

    /// Map preset index to a distinct indicator color.
    /// Cycles through 8 hues so each bank/preset is visually distinct.
    fn preset_color(index: u8) -> crate::led::Rgb {
        match index % 8 {
            0 => crate::led::Rgb::new(255, 0, 0),     // red
            1 => crate::led::Rgb::new(0, 255, 0),     // green
            2 => crate::led::Rgb::new(0, 0, 255),     // blue
            3 => crate::led::Rgb::new(255, 255, 0),   // yellow
            4 => crate::led::Rgb::new(255, 0, 255),   // magenta
            5 => crate::led::Rgb::new(0, 255, 255),   // cyan
            6 => crate::led::Rgb::new(255, 128, 0),   // orange
            7 => crate::led::Rgb::new(128, 0, 255),   // purple
            _ => crate::led::Rgb::new(255, 255, 255), // unreachable
        }
    }

    fn current_preset<'a, const A: usize>(
        &self,
        config: &'a Config<B, E, A>,
    ) -> Option<&'a Preset<B, E, A>> {
        config.presets.get(self.active_preset as usize)
    }

    fn handle_system_actions<const A: usize>(
        &mut self,
        result: &mut Output,
        now_ms: u32,
        config: &Config<B, E, A>,
    ) {
        let actions: heapless::Vec<SystemAction, 2> = result.pending_system.clone();
        result.pending_system.clear();
        for action in &actions {
            self.execute_system_action(*action, result, now_ms, config);
        }
    }

    fn process_button<const A: usize>(
        &mut self,
        index: usize,
        edge: Edge,
        now_ms: u32,
        config: &Config<B, E, A>,
    ) -> Output {
        let mut result = Output::new();

        let preset = match self.current_preset(config) {
            Some(p) => p,
            None => return result,
        };

        if index >= B {
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

    fn handle_gesture<const A: usize>(
        &mut self,
        index: usize,
        gesture: Gesture,
        preset: &Preset<B, E, A>,
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

    fn process_encoder<const A: usize>(
        &mut self,
        index: usize,
        clockwise: bool,
        now_ms: u32,
        config: &Config<B, E, A>,
    ) -> Output {
        let mut result = Output::new();
        let preset = match self.current_preset(config) {
            Some(p) => p,
            None => return result,
        };

        if index >= E {
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

    fn process_analog<const A: usize>(
        &mut self,
        index: usize,
        raw: u16,
        config: &Config<B, E, A>,
    ) -> Output {
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
        for s in &r.system {
            result.pending_system.push(*s).ok();
        }
    }

    fn execute_system_action<const A: usize>(
        &mut self,
        action: SystemAction,
        result: &mut Output,
        now_ms: u32,
        config: &Config<B, E, A>,
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

    fn do_switch_preset<const A: usize>(
        &mut self,
        new_idx: u8,
        result: &mut Output,
        config: &Config<B, E, A>,
    ) {
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
            self.encoder_accel = core::array::from_fn(|_| EncoderAccel::new());
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
        result.mode_led = Some(Self::preset_color(self.active_preset));
    }

    fn working_state(&self) -> PresetState<B, E> {
        let current = self.state_store.current();
        PresetState {
            button_active: self.button_active,
            cycle_index: current.cycle_index,
            encoder_values: current.encoder_values,
        }
    }

    fn apply_state(&mut self, state: &PresetState<B, E>) {
        self.button_active = state.button_active;
        self.state_store.save_working(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use heapless::Vec as HVec;

    // --- Helpers ---

    /// Build a minimal config with one preset containing the given buttons and encoders.
    fn make_config(
        buttons: HVec<ButtonConfig, MAX_BUTTONS>,
        encoders: HVec<EncoderConfig, MAX_ENCODERS>,
    ) -> Config {
        make_config_with_presets(buttons, encoders, HVec::new(), HVec::new(), HVec::new())
    }

    fn make_config_with_presets(
        buttons: HVec<ButtonConfig, MAX_BUTTONS>,
        encoders: HVec<EncoderConfig, MAX_ENCODERS>,
        on_enter: HVec<Action, MAX_ACTIONS>,
        on_exit: HVec<Action, MAX_ACTIONS>,
        triggers: HVec<Trigger, MAX_TRIGGERS>,
    ) -> Config {
        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();
        presets
            .push(Preset {
                name: Label::try_from("P1").unwrap(),
                buttons,
                encoders,
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter,
                on_exit,
                triggers,
            })
            .ok();
        Config {
            global: GlobalConfig::default(),
            presets,
        }
    }

    fn make_two_preset_config() -> Config {
        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();

        // Preset 0: one toggle button with CC#80
        let mut buttons0: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        let mut on_press0: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press0.push(Action::cc(80, 127, 1).unwrap()).ok();
        let mut on_release0: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_release0.push(Action::cc(80, 0, 1).unwrap()).ok();
        buttons0
            .push(ButtonConfig {
                label: Label::try_from("FX1").unwrap(),
                color: LedConfig::default(),
                mode: ButtonMode::Toggle,
                on_press: on_press0,
                on_release: on_release0,
                on_long_press: HVec::new(),
                cycle_values: HVec::new(),
                listen_cc: None,
            })
            .ok();

        let mut on_enter0: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_enter0.push(Action::program_change(0, 1).unwrap()).ok();

        presets
            .push(Preset {
                name: Label::try_from("Preset0").unwrap(),
                buttons: buttons0,
                encoders: HVec::new(),
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter: on_enter0,
                on_exit: HVec::new(),
                triggers: HVec::new(),
            })
            .ok();

        // Preset 1: one momentary button with CC#81
        let mut buttons1: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        let mut on_press1: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press1.push(Action::cc(81, 127, 1).unwrap()).ok();
        buttons1
            .push(ButtonConfig {
                label: Label::try_from("FX2").unwrap(),
                color: LedConfig::default(),
                mode: ButtonMode::Momentary,
                on_press: on_press1,
                on_release: HVec::new(),
                on_long_press: HVec::new(),
                cycle_values: HVec::new(),
                listen_cc: None,
            })
            .ok();

        let mut on_enter1: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_enter1.push(Action::program_change(1, 1).unwrap()).ok();

        presets
            .push(Preset {
                name: Label::try_from("Preset1").unwrap(),
                buttons: buttons1,
                encoders: HVec::new(),
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter: on_enter1,
                on_exit: HVec::new(),
                triggers: HVec::new(),
            })
            .ok();

        Config {
            global: GlobalConfig::default(),
            presets,
        }
    }

    fn make_three_preset_config() -> Config {
        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();
        for i in 0..3u8 {
            let mut on_enter: HVec<Action, MAX_ACTIONS> = HVec::new();
            on_enter.push(Action::program_change(i, 1).unwrap()).ok();
            presets
                .push(Preset {
                    name: Label::try_from("P").unwrap(),
                    buttons: HVec::new(),
                    encoders: HVec::new(),
                    analog: HVec::new(),
                    defaults: Default::default(),
                    on_enter,
                    on_exit: HVec::new(),
                    triggers: HVec::new(),
                })
                .ok();
        }
        Config {
            global: GlobalConfig::default(),
            presets,
        }
    }

    fn momentary_button(
        on_press: HVec<Action, MAX_ACTIONS>,
        on_release: HVec<Action, MAX_ACTIONS>,
    ) -> ButtonConfig {
        ButtonConfig {
            label: Label::new(),
            color: LedConfig::default(),
            mode: ButtonMode::Momentary,
            on_press,
            on_release,
            on_long_press: HVec::new(),
            cycle_values: HVec::new(),
            listen_cc: None,
        }
    }

    fn momentary_button_with_long_press(
        on_press: HVec<Action, MAX_ACTIONS>,
        on_long_press: HVec<Action, MAX_ACTIONS>,
    ) -> ButtonConfig {
        ButtonConfig {
            label: Label::new(),
            color: LedConfig::default(),
            mode: ButtonMode::Momentary,
            on_press,
            on_release: HVec::new(),
            on_long_press,
            cycle_values: HVec::new(),
            listen_cc: None,
        }
    }

    fn has_midi_msg(output: &Output, expected: [u8; 3]) -> bool {
        output
            .midi
            .iter()
            .any(|step| matches!(step, ActionStep::Send(m) if m.data == expected))
    }

    // --- Test 1: Simple button press generates MIDI (no long-press configured) ---

    #[test]
    fn simple_button_press_generates_midi() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons.push(momentary_button(on_press, HVec::new())).ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        assert!(has_midi_msg(&result, [0xB0, 80, 127]));
        assert!(result.leds_changed);
    }

    // --- Test 2: Button release generates MIDI ---

    #[test]
    fn button_release_generates_midi() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();
        let mut on_release: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_release.push(Action::cc(80, 0, 1).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons.push(momentary_button(on_press, on_release)).ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        // Press
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        // Release
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Deactivate,
            },
            100,
            &config,
        );

        assert!(has_midi_msg(&result, [0xB0, 80, 0]));
    }

    // --- Test 3: Short press with long-press configured ---

    #[test]
    fn short_press_with_long_press_configured_fires_on_press_on_release() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();
        let mut on_long_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_long_press.push(Action::PresetNext).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(momentary_button_with_long_press(on_press, on_long_press))
            .ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        // Press at t=0
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );
        // With long-press configured, press is deferred — no MIDI yet
        assert!(!has_midi_msg(&result, [0xB0, 80, 127]));

        // Tick at t=200 (under 500ms threshold)
        let result = ctrl.process(Event::Tick, 200, &config);
        assert!(result.midi.is_empty());

        // Release at t=300 (<500ms = short press)
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Deactivate,
            },
            300,
            &config,
        );

        // Short press fires on_press actions
        assert!(has_midi_msg(&result, [0xB0, 80, 127]));
    }

    // --- Test 4: Long press fires on_long_press actions ---

    #[test]
    fn long_press_fires_on_long_press_actions() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();
        let mut on_long_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_long_press.push(Action::cc(99, 127, 2).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(momentary_button_with_long_press(on_press, on_long_press))
            .ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        // Press at t=0
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        // Tick at t=500 (threshold reached)
        let result = ctrl.process(Event::Tick, 500, &config);

        // Long press fires on_long_press: CC#99 on ch2
        assert!(has_midi_msg(&result, [0xB1, 99, 127]));
    }

    // --- Test 5: Long press suppresses short press ---

    #[test]
    fn long_press_suppresses_short_press_on_release() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();
        let mut on_long_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_long_press.push(Action::cc(99, 127, 2).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(momentary_button_with_long_press(on_press, on_long_press))
            .ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        // Press at t=0
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        // Long press fires at t=500
        ctrl.process(Event::Tick, 500, &config);

        // Release after long press
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Deactivate,
            },
            600,
            &config,
        );

        // Release should NOT fire on_press (suppressed)
        assert!(!has_midi_msg(&result, [0xB0, 80, 127]));
        assert!(result.midi.is_empty());
    }

    // --- Test 6: Long press triggers preset switch ---

    #[test]
    fn long_press_triggers_preset_switch() {
        let mut on_long_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_long_press.push(Action::PresetNext).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(momentary_button_with_long_press(HVec::new(), on_long_press))
            .ok();

        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();
        presets
            .push(Preset {
                name: Label::try_from("P1").unwrap(),
                buttons,
                encoders: HVec::new(),
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter: HVec::new(),
                on_exit: HVec::new(),
                triggers: HVec::new(),
            })
            .ok();
        presets
            .push(Preset {
                name: Label::try_from("P2").unwrap(),
                buttons: HVec::new(),
                encoders: HVec::new(),
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter: HVec::new(),
                on_exit: HVec::new(),
                triggers: HVec::new(),
            })
            .ok();
        let config: Config = Config {
            global: GlobalConfig::default(),
            presets,
        };
        let mut ctrl = Controller::new();

        assert_eq!(ctrl.active_preset(), 0);

        // Press at t=0
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        // Long press fires at t=500 → PresetNext
        let result = ctrl.process(Event::Tick, 500, &config);

        assert!(result.preset_changed);
        assert_eq!(ctrl.active_preset(), 1);
    }

    // --- Test 7: Toggle state preserved across preset switch round-trip ---

    #[test]
    fn toggle_state_preserved_across_preset_switch() {
        let config = make_two_preset_config();
        let mut ctrl = Controller::new();

        // Toggle button 0 ON in preset 0 (no long-press → immediate fire)
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );
        assert!(has_midi_msg(&result, [0xB0, 80, 127]));
        assert!(ctrl.button_states()[0]);

        // Switch to preset 1
        ctrl.select_preset(1, &config);
        assert_eq!(ctrl.active_preset(), 1);
        // Button state is for preset 1 now
        assert!(!ctrl.button_states()[0]);

        // Switch back to preset 0
        ctrl.select_preset(0, &config);
        assert_eq!(ctrl.active_preset(), 0);
        // Toggle state should be restored
        assert!(ctrl.button_states()[0]);
    }

    // --- Test 8: Encoder turn generates CC ---

    #[test]
    fn encoder_turn_generates_cc() {
        let mut encoders: HVec<EncoderConfig, MAX_ENCODERS> = HVec::new();
        encoders
            .push(EncoderConfig {
                label: Label::try_from("Vol").unwrap(),
                action: EncoderAction::Cc {
                    cc: 7,
                    channel: 1,
                    min: 0,
                    max: 127,
                },
                ..Default::default()
            })
            .ok();

        let config = make_config(HVec::new(), encoders);
        let mut ctrl = Controller::new();
        ctrl.set_encoder_value(0, 64);

        let result = ctrl.process(
            Event::EncoderTurn {
                index: 0,
                clockwise: true,
            },
            1000,
            &config,
        );

        // First turn always gives 1 step (no prior timestamp for acceleration)
        assert!(has_midi_msg(&result, [0xB0, 7, 65]));
        assert_eq!(ctrl.encoder_values()[0], 65);
    }

    // --- Test 9: Encoder acceleration (fast turns → bigger steps) ---

    #[test]
    fn encoder_acceleration_fast_turns_bigger_steps() {
        let mut encoders: HVec<EncoderConfig, MAX_ENCODERS> = HVec::new();
        encoders
            .push(EncoderConfig {
                label: Label::try_from("Vol").unwrap(),
                action: EncoderAction::Cc {
                    cc: 7,
                    channel: 1,
                    min: 0,
                    max: 127,
                },
                ..Default::default()
            })
            .ok();

        let config = make_config(HVec::new(), encoders);
        let mut ctrl = Controller::new();
        ctrl.set_encoder_value(0, 50);

        // First turn at t=0 (initializes, returns 1 step)
        ctrl.process(
            Event::EncoderTurn {
                index: 0,
                clockwise: true,
            },
            0,
            &config,
        );
        assert_eq!(ctrl.encoder_values()[0], 51);

        // Fast turn at t=30 (30ms interval → 4 steps from accel_curve)
        let result = ctrl.process(
            Event::EncoderTurn {
                index: 0,
                clockwise: true,
            },
            30,
            &config,
        );
        // 51 + 4 = 55
        assert_eq!(ctrl.encoder_values()[0], 55);
        assert!(has_midi_msg(&result, [0xB0, 7, 55]));
    }

    // --- Test 10: Tap tempo via Action::TapTempo ---

    #[test]
    fn tap_tempo_fires_bpm() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::TapTempo).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons.push(momentary_button(on_press, HVec::new())).ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        // First tap at t=0 — no BPM yet (need 4 taps)
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );
        assert_eq!(result.bpm, None);

        // Taps 2 and 3 — still no BPM
        for t in [500, 1000] {
            ctrl.process(
                Event::ButtonEdge {
                    index: 0,
                    edge: Edge::Deactivate,
                },
                t - 50,
                &config,
            );
            let result = ctrl.process(
                Event::ButtonEdge {
                    index: 0,
                    edge: Edge::Activate,
                },
                t,
                &config,
            );
            assert_eq!(result.bpm, None);
        }

        // Fourth tap at t=1500 → 120 BPM
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Deactivate,
            },
            1450,
            &config,
        );
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            1500,
            &config,
        );
        assert_eq!(result.bpm, Some(120));
    }

    // --- Test 11: Midi triggers (CC trigger activates button) ---

    #[test]
    fn incoming_midi_trigger_activates_button() {
        let mut triggers: HVec<Trigger, MAX_TRIGGERS> = HVec::new();
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

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Toggle,
                on_press: HVec::new(),
                on_release: HVec::new(),
                on_long_press: HVec::new(),
                cycle_values: HVec::new(),
                listen_cc: None,
            })
            .ok();

        let config =
            make_config_with_presets(buttons, HVec::new(), HVec::new(), HVec::new(), triggers);
        let mut ctrl = Controller::new();

        assert!(!ctrl.button_states()[0]);

        // Send CC#100 = 127 on channel 1
        let result = ctrl.process(
            Event::Midi {
                data: [0xB0, 100, 127, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            0,
            &config,
        );

        assert!(ctrl.button_states()[0]);
        assert!(result.leds_changed);
    }

    // --- Test 12: Tick is no-op when no button held ---

    #[test]
    fn tick_is_noop_when_no_button_held() {
        let mut on_long_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_long_press.push(Action::cc(99, 127, 1).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(momentary_button_with_long_press(HVec::new(), on_long_press))
            .ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        // Tick without any button held
        let result = ctrl.process(Event::Tick, 1000, &config);

        assert!(result.midi.is_empty());
        assert!(!result.leds_changed);
        assert!(!result.preset_changed);
        assert_eq!(result.bpm, None);
    }

    // --- Test 13: select_preset fires on_enter MIDI ---

    #[test]
    fn select_preset_fires_on_enter_midi() {
        let config = make_two_preset_config();
        let mut ctrl = Controller::new();

        // Select preset 1 (has on_enter: PC#1 on ch1)
        let result = ctrl.select_preset(1, &config);

        assert!(result.preset_changed);
        assert_eq!(ctrl.active_preset(), 1);
        // on_enter for preset 1: Program Change 1 on channel 1
        assert!(has_midi_msg(&result, [0xC0, 1, 0]));
    }

    // --- Test 14: button_held() tracks press/release ---

    #[test]
    fn button_held_tracks_press_release() {
        // Must use a button with long-press configured (long-press detector tracks active state)
        let mut on_long_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_long_press.push(Action::cc(99, 127, 1).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(momentary_button_with_long_press(HVec::new(), on_long_press))
            .ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        assert!(!ctrl.button_held());

        // Press
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );
        assert!(ctrl.button_held());

        // Release
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Deactivate,
            },
            100,
            &config,
        );
        assert!(!ctrl.button_held());
    }

    // --- Test 15: Preset prev wraps around ---

    #[test]
    fn preset_prev_wraps_around() {
        // Build config with PresetPrev button in all 3 presets
        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();
        for i in 0..3u8 {
            let mut btn_on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
            btn_on_press.push(Action::PresetPrev).ok();
            let mut btns: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
            btns.push(momentary_button(btn_on_press, HVec::new())).ok();

            let mut on_enter: HVec<Action, MAX_ACTIONS> = HVec::new();
            on_enter.push(Action::program_change(i, 1).unwrap()).ok();

            presets
                .push(Preset {
                    name: Label::try_from("P").unwrap(),
                    buttons: btns,
                    encoders: HVec::new(),
                    analog: HVec::new(),
                    defaults: Default::default(),
                    on_enter,
                    on_exit: HVec::new(),
                    triggers: HVec::new(),
                })
                .ok();
        }
        let config: Config = Config {
            global: GlobalConfig::default(),
            presets,
        };
        let mut ctrl = Controller::new();

        assert_eq!(ctrl.active_preset(), 0);

        // Press button → PresetPrev wraps from 0 → 2
        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        assert!(result.preset_changed);
        assert_eq!(ctrl.active_preset(), 2);
    }

    // --- Additional edge-case tests ---

    #[test]
    fn button_held_false_for_button_without_long_press() {
        // A button without on_long_press doesn't use the long-press detector
        // in "active" mode, so button_held() should remain false.
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons.push(momentary_button(on_press, HVec::new())).ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        // Without long-press, the detector doesn't track "active" — button_held returns false
        assert!(!ctrl.button_held());
    }

    #[test]
    fn select_preset_fires_on_exit_of_old_preset() {
        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();

        let mut on_exit: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_exit.push(Action::cc(123, 0, 1).unwrap()).ok(); // All Notes Off

        presets
            .push(Preset {
                name: Label::try_from("P1").unwrap(),
                buttons: HVec::new(),
                encoders: HVec::new(),
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter: HVec::new(),
                on_exit,
                triggers: HVec::new(),
            })
            .ok();

        let mut on_enter: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_enter.push(Action::program_change(5, 1).unwrap()).ok();

        presets
            .push(Preset {
                name: Label::try_from("P2").unwrap(),
                buttons: HVec::new(),
                encoders: HVec::new(),
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter,
                on_exit: HVec::new(),
                triggers: HVec::new(),
            })
            .ok();

        let config: Config = Config {
            global: GlobalConfig::default(),
            presets,
        };
        let mut ctrl = Controller::new();

        let result = ctrl.select_preset(1, &config);

        // on_exit of preset 0 should fire: CC#123=0 (All Notes Off)
        assert!(has_midi_msg(&result, [0xB0, 123, 0]));
        // on_enter of preset 1 should fire: PC#5
        assert!(has_midi_msg(&result, [0xC0, 5, 0]));
    }

    #[test]
    fn encoder_values_persist_across_preset_switch() {
        let mut encoders: HVec<EncoderConfig, MAX_ENCODERS> = HVec::new();
        encoders
            .push(EncoderConfig {
                label: Label::try_from("Vol").unwrap(),
                action: EncoderAction::Cc {
                    cc: 7,
                    channel: 1,
                    min: 0,
                    max: 127,
                },
                ..Default::default()
            })
            .ok();

        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();
        presets
            .push(Preset {
                name: Label::try_from("P1").unwrap(),
                buttons: HVec::new(),
                encoders: encoders.clone(),
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter: HVec::new(),
                on_exit: HVec::new(),
                triggers: HVec::new(),
            })
            .ok();
        presets
            .push(Preset {
                name: Label::try_from("P2").unwrap(),
                buttons: HVec::new(),
                encoders,
                analog: HVec::new(),
                defaults: Default::default(),
                on_enter: HVec::new(),
                on_exit: HVec::new(),
                triggers: HVec::new(),
            })
            .ok();

        let config: Config = Config {
            global: GlobalConfig::default(),
            presets,
        };
        let mut ctrl = Controller::new();
        ctrl.set_encoder_value(0, 80);

        // Switch to preset 1
        ctrl.select_preset(1, &config);
        assert_eq!(ctrl.encoder_values()[0], 0); // fresh preset 1 state

        // Switch back to preset 0
        ctrl.select_preset(0, &config);
        assert_eq!(ctrl.encoder_values()[0], 80); // restored
    }

    #[test]
    fn snapshot_store_captures_current_state() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(80, 127, 1).unwrap()).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Toggle,
                on_press,
                on_release: HVec::new(),
                on_long_press: HVec::new(),
                cycle_values: HVec::new(),
                listen_cc: None,
            })
            .ok();

        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();
        ctrl.set_encoder_value(0, 42);

        // Toggle button on
        ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config,
        );

        let store = ctrl.snapshot_store();
        let state = store.current();
        assert!(state.button_active[0]);
        assert_eq!(state.encoder_values[0], 42);
    }

    #[test]
    fn incoming_midi_trigger_below_value_min_no_match() {
        let mut triggers: HVec<Trigger, MAX_TRIGGERS> = HVec::new();
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

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons
            .push(ButtonConfig {
                label: Label::new(),
                color: LedConfig::default(),
                mode: ButtonMode::Toggle,
                on_press: HVec::new(),
                on_release: HVec::new(),
                on_long_press: HVec::new(),
                cycle_values: HVec::new(),
                listen_cc: None,
            })
            .ok();

        let config =
            make_config_with_presets(buttons, HVec::new(), HVec::new(), HVec::new(), triggers);
        let mut ctrl = Controller::new();

        // Send CC#100 = 63 (below value_min=64)
        let result = ctrl.process(
            Event::Midi {
                data: [0xB0, 100, 63, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            0,
            &config,
        );

        assert!(!ctrl.button_states()[0]);
        assert!(!result.leds_changed);
    }

    #[test]
    fn preset_next_wraps_around() {
        let config = make_three_preset_config();
        let mut ctrl = Controller::new();

        // Go to preset 2 first
        ctrl.select_preset(2, &config);
        assert_eq!(ctrl.active_preset(), 2);

        // Now process PresetNext via a button
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::PresetNext).ok();

        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons.push(momentary_button(on_press, HVec::new())).ok();

        let mut presets: HVec<Preset, MAX_PRESETS> = HVec::new();
        for _i in 0..3u8 {
            let mut btns: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
            let mut bp: HVec<Action, MAX_ACTIONS> = HVec::new();
            bp.push(Action::PresetNext).ok();
            btns.push(momentary_button(bp, HVec::new())).ok();
            presets
                .push(Preset {
                    name: Label::try_from("P").unwrap(),
                    buttons: btns,
                    encoders: HVec::new(),
                    analog: HVec::new(),
                    defaults: Default::default(),
                    on_enter: HVec::new(),
                    on_exit: HVec::new(),
                    triggers: HVec::new(),
                })
                .ok();
        }
        let config2 = Config {
            global: GlobalConfig::default(),
            presets,
        };
        let mut ctrl2 = Controller::new();
        ctrl2.select_preset(2, &config2);

        let result = ctrl2.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            0,
            &config2,
        );

        assert!(result.preset_changed);
        assert_eq!(ctrl2.active_preset(), 0); // wraps from 2 → 0
    }

    #[test]
    fn mon_led_green_on_midi_output() {
        let mut on_press: HVec<Action, MAX_ACTIONS> = HVec::new();
        on_press.push(Action::cc(10, 127, 1).unwrap()).ok();
        let mut buttons: HVec<ButtonConfig, MAX_BUTTONS> = HVec::new();
        buttons.push(momentary_button(on_press, HVec::new())).ok();
        let config = make_config(buttons, HVec::new());
        let mut ctrl = Controller::new();

        let result = ctrl.process(
            Event::ButtonEdge {
                index: 0,
                edge: Edge::Activate,
            },
            100,
            &config,
        );
        assert!(!result.midi.is_empty());
        assert_eq!(result.mon_led, Some(crate::led::Rgb::new(0, 255, 0)));
    }

    #[test]
    fn mon_led_blue_on_incoming_midi() {
        let config = make_config(HVec::new(), HVec::new());
        let mut ctrl = Controller::new();

        let result = ctrl.process(
            Event::Midi {
                data: [0xB0, 10, 127, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            100,
            &config,
        );
        assert_eq!(result.mon_led, Some(crate::led::Rgb::new(0, 0, 255)));
    }

    #[test]
    fn mode_led_set_on_preset_change() {
        let config = make_three_preset_config();
        let mut ctrl = Controller::new();

        let result = ctrl.select_preset(1, &config);
        assert!(result.preset_changed);
        // Preset 1 = green (index 1 in the color table)
        assert_eq!(result.mode_led, Some(crate::led::Rgb::new(0, 255, 0)));
    }

    #[test]
    fn mon_led_none_on_tick() {
        let config = make_config(HVec::new(), HVec::new());
        let mut ctrl = Controller::new();
        let result = ctrl.process(Event::Tick, 100, &config);
        assert_eq!(result.mon_led, None);
        assert_eq!(result.mode_led, None);
    }

    // --- Test: Thru routing (DIN → USB when enabled) ---
    #[test]
    fn thru_din_to_usb_enabled() {
        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        config.global.din_to_usb_thru = true;
        config.presets.push(Preset::default()).ok();

        let result = ctrl.process(
            Event::Midi {
                data: [0x90, 60, 100, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::DIN,
            },
            0,
            &config,
        );

        assert_eq!(result.midi_out.len(), 1);
        assert_eq!(result.midi_out[0].bytes(), &[0x90, 60, 100]);
        assert_eq!(result.midi_out[0].dest, MidiPort::USB);
    }

    #[test]
    fn thru_din_to_usb_disabled() {
        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        config.global.din_to_usb_thru = false;
        config.presets.push(Preset::default()).ok();

        let result = ctrl.process(
            Event::Midi {
                data: [0x90, 60, 100, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::DIN,
            },
            0,
            &config,
        );

        assert!(result.midi_out.is_empty());
    }

    #[test]
    fn thru_usb_to_din_enabled() {
        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        config.global.usb_to_din_thru = true;
        config.presets.push(Preset::default()).ok();

        let result = ctrl.process(
            Event::Midi {
                data: [0xB0, 7, 64, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            0,
            &config,
        );

        assert_eq!(result.midi_out.len(), 1);
        assert!(result.midi_out[0].dest.contains(MidiPort::DIN));
        assert!(!result.midi_out[0].dest.contains(MidiPort::USB));
    }

    #[test]
    fn thru_usb_to_both() {
        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        config.global.usb_to_din_thru = true;
        config.global.usb_to_usb_thru = true;
        config.presets.push(Preset::default()).ok();

        let result = ctrl.process(
            Event::Midi {
                data: [0xB0, 1, 127, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            0,
            &config,
        );

        assert_eq!(result.midi_out.len(), 1);
        assert!(result.midi_out[0].dest.contains(MidiPort::DIN));
        assert!(result.midi_out[0].dest.contains(MidiPort::USB));
    }

    #[test]
    fn thru_no_routing_when_all_disabled() {
        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        config.global.din_to_usb_thru = false;
        config.global.usb_to_din_thru = false;
        config.global.usb_to_usb_thru = false;
        config.presets.push(Preset::default()).ok();

        let result = ctrl.process(
            Event::Midi {
                data: [0x90, 48, 80, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::DIN,
            },
            0,
            &config,
        );

        assert!(result.midi_out.is_empty());
    }

    #[test]
    fn incoming_midi_sets_mon_led_blue() {
        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        config.presets.push(Preset::default()).ok();

        let result = ctrl.process(
            Event::Midi {
                data: [0x90, 60, 100, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            0,
            &config,
        );

        // No triggers → no MIDI output → Mon LED should be blue (incoming processed)
        assert_eq!(result.mon_led, Some(crate::led::Rgb::new(0, 0, 255)));
    }

    #[test]
    fn reactive_led_heatmap_on_matching_cc() {
        use crate::config::{ButtonConfig, ButtonMode, Label, LedConfig, ListenCc, ListenMode};
        use crate::engine::ReactiveResult;
        use heapless::Vec as HVec;

        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        let mut preset = Preset::default();
        preset.name = heapless::String::try_from("Test").unwrap();
        let btn = ButtonConfig {
            label: Label::new(),
            color: LedConfig::default(),
            mode: ButtonMode::Toggle,
            on_press: HVec::new(),
            on_release: HVec::new(),
            on_long_press: HVec::new(),
            cycle_values: HVec::new(),
            listen_cc: Some(ListenCc {
                cc: 7,
                channel: 1,
                mode: ListenMode::Heatmap,
                threshold: 64,
            }),
        };
        preset.buttons.push(btn).ok();
        config.presets.push(preset).ok();

        let result = ctrl.process(
            Event::Midi {
                data: [0xB0, 7, 127, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::DIN,
            },
            0,
            &config,
        );

        assert_eq!(result.reactive_led, Some(ReactiveResult::Heatmap(0, 12)));
    }

    #[test]
    fn reactive_led_trigger_on_matching_cc() {
        use crate::config::{ButtonConfig, ButtonMode, Label, LedConfig, ListenCc, ListenMode};
        use crate::engine::ReactiveResult;
        use heapless::Vec as HVec;

        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        let mut preset = Preset::default();
        preset.name = heapless::String::try_from("Test").unwrap();
        let btn = ButtonConfig {
            label: Label::new(),
            color: LedConfig::default(),
            mode: ButtonMode::Toggle,
            on_press: HVec::new(),
            on_release: HVec::new(),
            on_long_press: HVec::new(),
            cycle_values: HVec::new(),
            listen_cc: Some(ListenCc {
                cc: 50,
                channel: 2,
                mode: ListenMode::Trigger,
                threshold: 64,
            }),
        };
        preset.buttons.push(btn).ok();
        config.presets.push(preset).ok();

        // Value above threshold → active
        let result = ctrl.process(
            Event::Midi {
                data: [0xB1, 50, 100, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            0,
            &config,
        );
        assert_eq!(result.reactive_led, Some(ReactiveResult::Trigger(0, true)));

        // Value below threshold → inactive
        let result = ctrl.process(
            Event::Midi {
                data: [0xB1, 50, 10, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::USB,
            },
            0,
            &config,
        );
        assert_eq!(result.reactive_led, Some(ReactiveResult::Trigger(0, false)));
    }

    #[test]
    fn no_reactive_led_on_non_cc() {
        let mut ctrl = Controller::<6, 2>::new();
        let mut config: Config = Config::default();
        config.presets.push(Preset::default()).ok();

        // Note On — not a CC, no reactive LED
        let result = ctrl.process(
            Event::Midi {
                data: [0x90, 60, 127, 0, 0, 0, 0, 0],
                len: 3,
                source: MidiPort::DIN,
            },
            0,
            &config,
        );
        assert_eq!(result.reactive_led, None);
    }
}
