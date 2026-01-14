//! Firmware BIOS PCI config-space adapters.
//!
//! `aero-machine` historically carried its own copy of the firmware PCI config-space adapter used
//! during BIOS POST. The implementation now lives in `aero-pci-firmware-adapter`; this module is a
//! crate-private re-export to keep internal module paths stable.

pub(crate) use aero_pci_firmware_adapter::SharedPciConfigPortsBiosAdapter;
