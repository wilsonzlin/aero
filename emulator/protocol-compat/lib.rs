//! Compatibility shim crate.
//!
//! The canonical AeroGPU protocol bindings live in the `aero-protocol` package at
//! `emulator/protocol/`. Some external scripts and historical task specs refer to
//! this crate as `emulator-protocol`; this package exists to keep those entrypoints
//! working without renaming the canonical crate.

pub use aero_protocol::*;
