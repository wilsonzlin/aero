//! D3D9 shader helpers.
//!
//! This crate currently exists as a lightweight wrapper around shared shader parsing logic.
//! The main D3D9 runtime uses `crates/aero-d3d9/src/sm3` directly, but the workspace includes
//! `aero-d3d9-shader` to keep shader-related utilities in a dedicated crate when needed.

pub use aero_dxbc;
