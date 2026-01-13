//! EHCI (USB 2.0) controller PCI wrapper.
//!
//! This module currently provides the PCI identity and BAR layout for a Windows-7-friendly EHCI
//! controller function. The full controller implementation is introduced in a follow-up change
//! (EHCI-007); until then, MMIO reads return all 1s and writes are ignored.

use crate::pci::profile::USB_EHCI_ICH9;
use crate::pci::{PciConfigSpace, PciDevice};

/// PCI wrapper for an emulated EHCI controller.
///
/// This device exposes an Intel ICH9-family EHCI PCI identity (widely supported by Windows 7 inbox
/// drivers), including:
/// - class code 0x0c0320 (serial bus / USB / EHCI)
/// - BAR0 MMIO window size 0x1000
/// - interrupt pin INTA#
pub struct EhciPciDevice {
    config: PciConfigSpace,
}

impl EhciPciDevice {
    /// EHCI MMIO register block size (BAR0).
    pub const MMIO_BAR_SIZE: u64 = 0x1000;
    /// EHCI MMIO BAR index (BAR0).
    pub const MMIO_BAR_INDEX: u8 = 0;

    pub fn new() -> Self {
        let config = USB_EHCI_ICH9.build_config_space();
        Self { config }
    }
}

impl Default for EhciPciDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl PciDevice for EhciPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

impl memory::MmioHandler for EhciPciDevice {
    fn read(&mut self, _offset: u64, size: usize) -> u64 {
        all_ones(size)
    }

    fn write(&mut self, _offset: u64, _size: usize, _value: u64) {
        // No-op until the controller model is implemented.
    }
}

fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}
