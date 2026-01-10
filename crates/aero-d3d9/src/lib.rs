//! Direct3D 9 → WebGPU translation primitives.
//!
//! This crate is intentionally self-contained so it can be used both in the
//! emulator and in host-side test harnesses.

pub mod abi;
pub mod dxbc;
pub mod shader;
pub mod software;
pub mod state;
pub mod sm3;

#[cfg(test)]
mod tests;
