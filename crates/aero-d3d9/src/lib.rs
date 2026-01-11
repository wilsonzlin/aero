//! Direct3D 9 → WebGPU translation primitives.
//!
//! This crate is intentionally self-contained so it can be used both in the
//! emulator and in host-side test harnesses.
//!
//! The [`resources`] module provides a D3D9-ish resource management layer (VB/IB/textures/samplers
//! and RT/DS surfaces) mapped to `wgpu` objects with lock/unlock update semantics.

pub mod abi;
pub mod dxbc;
pub mod fixed_function;
pub mod resources;
pub mod runtime;
pub mod shader;
pub mod sm3;
pub mod software;
pub mod state;
pub mod vertex;

#[cfg(test)]
mod tests;
