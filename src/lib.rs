#![no_std]

pub mod action;
pub mod config;
pub mod controller;
pub mod encoder_accel;
pub mod engine;
pub mod led;
pub mod long_press;
pub mod property_exchange;
pub mod routing;
pub mod state;
pub mod tap_tempo;

// Re-export default type aliases for convenience.
pub use config::{DefaultConfig, DefaultPreset};
pub use controller::DefaultController;
pub use state::{DefaultPresetState, DefaultPresetStateStore};
