//! Compatibility shim for the former `aero-gpu-device` crate.
//!
//! The project currently hosts the guest memory abstraction in `aero-gpu`, but
//! some components (e.g. `aero-d3d11`) still depend on the historical crate
//! name. Keep this crate minimal and re-export the types expected by those
//! callers.

pub mod guest_memory {
    pub use aero_gpu::{GuestMemory, GuestMemoryError, VecGuestMemory};
}

