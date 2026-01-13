//! AeroGPU device-side helpers.
//!
//! This crate intentionally focuses on the "hardware" view of the AeroGPU device model
//! (MMIO/shared-memory protocols), while still providing the abstraction boundary between:
//! - the device-model side (rings, guest memory), and
//! - the host-side GPU command executor.
//!
//! The [`executor`] module contains the ring executor responsible for doorbell processing, fence
//! tracking, and vblank pacing.
//!
//! The [`pci`] module provides a BAR1-backed VRAM aperture that can also be aliased into the
//! legacy VGA (`0xA0000..0xC0000`) and VBE linear framebuffer mappings.
#![forbid(unsafe_code)]

pub mod backend;
pub mod pci;
pub mod executor;
pub mod regs;
pub mod ring;
pub mod scanout;

pub use backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend, ImmediateAeroGpuBackend, NullAeroGpuBackend,
};
pub use memory::MemoryBus;
pub use pci::{
    AeroGpuBar1VramMmio, AeroGpuPciDevice, LEGACY_VGA_PADDR_BASE, LEGACY_VGA_PADDR_END,
    LEGACY_VGA_VRAM_BYTES, VBE_LFB_OFFSET,
};
pub use scanout::{AeroGpuCursorConfig, AeroGpuFormat, AeroGpuScanoutConfig};
