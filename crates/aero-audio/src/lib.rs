//! Aero audio subsystem.
//!
//! This crate currently provides a minimal Intel HD Audio (HDA) controller + codec
//! implementation along with helpers for feeding a Web Audio `AudioWorkletProcessor`
//! via a `SharedArrayBuffer` ring buffer.

pub mod hda;
pub mod clock;
pub mod mem;
pub mod pcm;
pub mod ring;
pub mod capture;
pub mod sink;

/// `SharedArrayBuffer` ring buffer layout used by the web `AudioWorkletProcessor`.
pub use aero_platform::audio::{mic_bridge, worklet_bridge};
