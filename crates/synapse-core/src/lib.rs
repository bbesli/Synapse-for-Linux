//! Core library for Synapse for Linux.
//!
//! * [`hid`] – raw HID access through `/dev/hidraw*` (no kernel module needed)
//! * [`devices`] – supported device models, discovery and the device drivers
//! * [`manager`] – background thread that keeps a device connected and polled
//! * [`config`] – persistent user configuration (TOML)
//! * [`eq`] – equalizer bands, hardware presets and built-in curves
//! * [`audio`] – PipeWire based microphone enhancement

pub mod audio;
pub mod config;
pub mod devices;
pub mod eq;
pub mod error;
pub mod hid;
pub mod manager;

pub use error::{Error, Result};
