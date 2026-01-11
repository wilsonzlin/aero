//! PCI-facing constants for AeroGPU.
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_pci.h` (mirrored in the `aero-protocol`
//! crate). These values are part of the guest↔host contract and should not drift.

/// AeroGPU PCI vendor ID.
pub const AEROGPU_PCI_VENDOR_ID: u16 = 0xA3A0;
/// AeroGPU PCI device ID.
pub const AEROGPU_PCI_DEVICE_ID: u16 = 0x0001;

/// BAR0 size for the MMIO register block.
pub const AEROGPU_MMIO_BAR_SIZE: u64 = 64 * 1024;

