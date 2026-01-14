//! Legacy AeroGPU PCI/MMIO device model ("ARGP").
//!
//! The implementation lives in the dedicated `aero-aerogpu` crate (feature-gated
//! behind `aero-aerogpu/legacy-abi`). The `emulator` crate keeps the public module
//! path for compatibility with existing tests and callers.

pub use aero_aerogpu::legacy::*;
