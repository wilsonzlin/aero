//! PCI-facing definitions for AeroGPU.
//!
//! The current repository doesn't yet have a full PCI configuration-space implementation.
//! This module exists to keep the device model "PCI-friendly": it defines stable IDs and BAR
//! sizing so the eventual PCI layer can wire them in without redesigning the GPU ABI.

/// AeroGPU PCI vendor ID (placeholder).
pub const AEROGPU_PCI_VENDOR_ID: u16 = 0x1AE0;
/// AeroGPU PCI device ID (placeholder).
pub const AEROGPU_PCI_DEVICE_ID: u16 = 0xE001;

/// BAR0 size for the MMIO register block.
pub const AEROGPU_MMIO_BAR_SIZE: u64 = 0x1000;
