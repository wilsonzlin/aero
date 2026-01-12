//! Cross-worker shared-memory protocols and layout definitions.
//!
//! The browser architecture uses `SharedArrayBuffer` + `Atomics` to exchange
//! data between the emulation core (CPU worker) and the GPU worker without
//! per-frame copies. This crate defines the in-memory layout and the atomic
//! publish protocol for the shared framebuffer.

pub mod scanout_state;
pub mod shared_framebuffer;
