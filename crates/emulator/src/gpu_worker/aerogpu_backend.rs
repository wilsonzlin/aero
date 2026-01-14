//! AeroGPU command backend abstraction used by the emulator's device model.
//!
//! This module re-exports the canonical backend API from `aero-devices-gpu` so the emulator and
//! the standalone device crates cannot drift.

pub use aero_devices_gpu::backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission, AeroGpuCommandBackend,
    ImmediateAeroGpuBackend, NullAeroGpuBackend,
};

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
pub use aero_devices_gpu::backend::NativeAeroGpuBackend;

