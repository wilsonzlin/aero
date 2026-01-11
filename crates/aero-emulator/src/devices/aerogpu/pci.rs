//! PCI-facing constants for AeroGPU.
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_pci.h` (mirrored in the `aero-protocol`
//! crate). These values are part of the guest↔host contract and should not drift.

use aero_protocol::aerogpu::aerogpu_pci as pci;

/// AeroGPU PCI vendor ID.
pub const AEROGPU_PCI_VENDOR_ID: u16 = pci::AEROGPU_PCI_VENDOR_ID;
/// AeroGPU PCI device ID.
pub const AEROGPU_PCI_DEVICE_ID: u16 = pci::AEROGPU_PCI_DEVICE_ID;

/// PCI base class code: Display controller.
pub const AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER: u8 =
    pci::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER;
/// PCI subclass: VGA-compatible controller.
pub const AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE: u8 = pci::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE;
/// PCI programming interface byte.
pub const AEROGPU_PCI_PROG_IF: u8 = pci::AEROGPU_PCI_PROG_IF;

/// BAR0 size for the MMIO register block.
pub const AEROGPU_MMIO_BAR_SIZE: u64 = pci::AEROGPU_PCI_BAR0_SIZE_BYTES as u64;
