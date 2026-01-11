//! Device-facing utilities shared by GPU protocol consumers.
//!
//! This crate currently exists as a small shim so higher-level GPU translation
//! layers (e.g. the D3D11 runtime) can depend on a stable guest-memory
//! abstraction without importing the full `aero-gpu` API surface.
//!
//! The types are re-exported from `aero-gpu` so all users share the same trait
//! definition.

pub mod guest_memory {
    pub use aero_gpu::{GuestMemory, GuestMemoryError, VecGuestMemory};
}

