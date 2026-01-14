#![forbid(unsafe_code)]

//! AeroGPU device-side helpers.
//!
//! This crate intentionally focuses on the "hardware" view of the AeroGPU device model
//! (PCI/MMIO/shared-memory protocols), while still providing the abstraction boundary between:
//! - the device-model side (rings, guest memory), and
//! - the host-side GPU command executor.
//!
//! The [`executor`] module contains the ring executor responsible for doorbell processing, fence
//! tracking, and vblank pacing.
//!
//! The main entry point is [`pci::AeroGpuPciDevice`], which exposes:
//! - a PCI config space image (used for gating MMIO/DMA/INTx),
//! - BAR0 MMIO register handling + vblank scheduling via an externally driven `now_ns` clock, and
//! - a BAR1-backed VRAM aperture that can also be aliased into the legacy VGA
//!   (`0xA0000..0xC0000`) and VBE linear framebuffer mappings.
//!
//! By default, the crate is GPU-free. Enable the `aerogpu-native` feature (or its compatibility
//! alias `wgpu-backend`) to execute command streams in-process via a WGPU/WebGPU-backed executor
//! (intended for native tests).

// The `aerogpu-native`/`wgpu-backend` feature enables a native in-process executor wrapper used by
// host-side tests. It is not intended to be enabled for `wasm32` builds; the browser runtime uses
// dedicated WASM/JS execution paths instead (e.g. `aero-gpu-wasm` + web workers).
#[cfg(all(feature = "aerogpu-native", target_arch = "wasm32"))]
compile_error!(
    "`aero-devices-gpu` feature `aerogpu-native`/`wgpu-backend` is not supported on wasm32; enable it only for native host builds/tests"
);

pub mod backend;
pub mod cmd;
pub mod device;
pub mod executor;
pub mod pci;
pub mod pci_device;
pub mod regs;
pub mod ring;
pub mod scanout;
pub mod vblank;

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
pub use backend::NativeAeroGpuBackend;
#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
pub use backend::NativeAeroGpuBackend as AerogpuWgpuBackend;
pub use backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend, ImmediateAeroGpuBackend, NullAeroGpuBackend,
};
pub use executor::{AeroGpuExecutor, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
pub use memory::MemoryBus;
pub use pci::{
    AeroGpuBar1VramMmio, AeroGpuDeviceConfig, AeroGpuLegacyVgaMmio, AeroGpuPciDevice,
    LEGACY_VGA_PADDR_BASE, LEGACY_VGA_PADDR_END, LEGACY_VGA_VRAM_BYTES, VBE_LFB_OFFSET,
};
pub use regs::{feature_bits, irq_bits, mmio, ring_control, AeroGpuRegs};
pub use ring::{
    AeroGpuAllocEntry, AeroGpuAllocTable, AeroGpuAllocTableError, AeroGpuAllocTableHeader,
    AeroGpuRingHeader, AeroGpuSubmitDesc, AeroGpuSubmitDescError, AEROGPU_ALLOC_TABLE_MAGIC,
    AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_FENCE_PAGE_SIZE_BYTES, AEROGPU_RING_HEADER_SIZE_BYTES,
    AEROGPU_RING_MAGIC, FENCE_PAGE_ABI_VERSION_OFFSET, FENCE_PAGE_COMPLETED_FENCE_OFFSET,
    FENCE_PAGE_MAGIC_OFFSET, RING_HEAD_OFFSET, RING_TAIL_OFFSET,
};
pub use scanout::{AeroGpuCursorConfig, AeroGpuFormat, AeroGpuScanoutConfig};
