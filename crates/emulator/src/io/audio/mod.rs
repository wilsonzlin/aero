//! Legacy audio subsystem (AC97/HDA/DSP + capture plumbing).
//!
//! This code path is retained for reference and targeted testing, but it is not
//! used by the browser/WASM runtime. The canonical audio stack lives in
//! `crates/aero-audio`, `crates/aero-virtio`, and `crates/platform::audio`.
//!
//! Enable the `emulator/legacy-audio` crate feature to compile this module.

pub mod ac97;
pub mod dsp;
pub mod hda;
pub mod input;
pub mod mixer;
