#![forbid(unsafe_code)]

//! AeroGPU device models and shared helpers.
//!
//! The canonical AeroGPU device model lives in the `emulator` crate today, but
//! we keep legacy/compat device models in a dedicated crate so consumers can
//! opt in via Cargo features without pulling the full emulator dependency
//! graph.

#[cfg(feature = "legacy-abi")]
pub mod legacy;
