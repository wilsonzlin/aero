//! Aero audio subsystem.
//!
//! This crate currently provides a minimal Intel HD Audio (HDA) controller + codec
//! implementation along with a ring buffer layout intended to be shared with a
//! Web Audio `AudioWorkletProcessor`.

pub mod hda;
pub mod mem;
pub mod pcm;
pub mod ring;

