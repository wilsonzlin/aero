//! AeroGPU device-side helpers.
//!
//! This crate intentionally focuses on the "hardware" view of the AeroGPU device model
//! (MMIO/shared-memory protocols), while still providing the abstraction boundary between:
//! - the device-model side (rings, guest memory), and
//! - the host-side GPU command executor.

pub mod backend;
pub mod ring;
pub mod scanout;

pub use backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission, AeroGpuCommandBackend,
    ImmediateAeroGpuBackend, NullAeroGpuBackend,
};
pub use memory::MemoryBus;
pub use scanout::{AeroGpuCursorConfig, AeroGpuFormat, AeroGpuScanoutConfig};
