//! Canonical Aero JIT crate.
//!
//! The implementation currently lives in the architecture-specific `aero-jit-x86` crate; this
//! crate exists as a stable import path (`aero_jit`) and forwards features and exports.

pub use aero_jit_x86::*;
