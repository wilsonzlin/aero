//! Aero audio subsystem.
//!
//! This crate currently provides a minimal Intel HD Audio (HDA) controller + codec
//! implementation along with helpers for feeding a Web Audio `AudioWorkletProcessor`
//! via a `SharedArrayBuffer` ring buffer.

pub mod capture;
pub mod clock;
pub mod hda;
pub mod hda_pci;
pub mod mem;
pub mod pcm;
pub mod ring;
pub mod sink;

/// `SharedArrayBuffer` ring buffer layout used by the web `AudioWorkletProcessor`.
pub use aero_platform::audio::{mic_bridge, worklet_bridge};

/// Defensive upper bound for host-provided sample rates.
///
/// Web Audio sample rates are typically 44.1kHz/48kHz (sometimes 96kHz). Since the emulator accepts
/// host-provided rates (e.g. from JS/WASM bindings) and may restore snapshot files from untrusted
/// sources, clamping prevents pathological allocations when sizing internal ring buffers and
/// resampler scratch space.
pub const MAX_HOST_SAMPLE_RATE_HZ: u32 = 384_000;
