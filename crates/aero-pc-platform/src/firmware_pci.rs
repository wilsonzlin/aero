//! Firmware BIOS PCI config-space adapters.
//!
//! This module is a thin re-export of the shared implementation in
//! `aero-pci-firmware-adapter` so downstream code can continue to use the historical
//! `aero_pc_platform::firmware_pci::*` paths.

pub use aero_pci_firmware_adapter::{PciConfigPortsBiosAdapter, SharedPciConfigPortsBiosAdapter};
