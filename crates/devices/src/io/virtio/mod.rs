//! Legacy virtio implementation.
//!
//! This module is kept for backwards compatibility, but it is **not** the
//! canonical virtio stack for this repository.
//!
//! New code should use the `aero_virtio` crate instead (used by `aero-machine`).
//!
//! The types in this module are deprecated to discourage introducing new
//! dependencies on this legacy stack.

pub mod core;
pub mod devices;
