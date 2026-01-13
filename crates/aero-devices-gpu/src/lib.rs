//! AeroGPU device-side helpers.
//!
//! This crate intentionally focuses on the "hardware" view of the AeroGPU
//! device model (MMIO/shared-memory protocols), and avoids pulling in the GPU
//! renderer itself.

pub mod ring;

pub use memory::MemoryBus;

