//! Aero IPC (inter-thread) primitives.
//!
//! This crate defines two things:
//! - A binary message protocol shared between TypeScript workers and Rust/WASM.
//! - A bounded, lock-free, variable-length ring buffer that can be placed in a
//!   `SharedArrayBuffer` and driven by `Atomics` / WASM atomics.
//!
//! The *layout contract* is documented in `docs/ipc-protocol.md`.

pub mod layout;
pub mod protocol;
pub mod ring;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
