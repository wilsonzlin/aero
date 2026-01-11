//! PCI-facing constants for AeroGPU.
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_pci.h` (mirrored in the `aero-protocol`
//! crate). These values are part of the guest↔host contract and should not drift.

/// AeroGPU PCI vendor ID.
pub const AEROGPU_PCI_VENDOR_ID: u16 = 0xA3A0;
/// AeroGPU PCI device ID.
pub const AEROGPU_PCI_DEVICE_ID: u16 = 0x0001;

/// PCI base class code: Display controller.
pub const AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER: u8 = 0x03;
/// PCI subclass: VGA-compatible controller.
pub const AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE: u8 = 0x00;
/// PCI programming interface byte.
pub const AEROGPU_PCI_PROG_IF: u8 = 0x00;

/// BAR0 size for the MMIO register block.
pub const AEROGPU_MMIO_BAR_SIZE: u64 = 64 * 1024;
