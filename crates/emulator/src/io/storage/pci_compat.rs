//! Compatibility helpers for bridging `aero_devices::pci::PciConfigSpace` behind the legacy
//! `crate::io::pci::PciDevice` interface.
//!
//! The legacy emulator-side PCI trait uses `config_read(&self, ...)`, but the canonical
//! `aero_devices::pci::PciConfigSpace` implementation requires `&mut self` for reads (it
//! synchronizes capability-backed bytes on demand).
//!
//! Additionally, `PciConfigSpace::write_with_effects` asserts that BAR writes are 32-bit aligned
//! and 32-bit wide. Hostile guests (or fuzz/tests) may issue subword writes into the BAR region,
//! which would otherwise panic. This module provides a small wrapper that performs a safe
//! read-modify-write for such accesses.

use aero_devices::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use aero_devices::pci::PciConfigSpace;
use std::cell::RefCell;

const PCI_BAR_OFF_START: u16 = 0x10;
const PCI_BAR_OFF_END: u16 = 0x27;

#[inline]
fn is_pci_bar_offset(offset: u16) -> bool {
    (PCI_BAR_OFF_START..=PCI_BAR_OFF_END).contains(&offset)
}

#[inline]
fn align_pci_dword(offset: u16) -> u16 {
    offset & !0x3
}

/// Wraps a canonical [`aero_devices::pci::PciConfigSpace`] to be safely accessed through the
/// emulator's legacy PCI config-space access pattern.
///
/// This wrapper:
/// - Provides `&self` reads via interior mutability.
/// - Converts subword BAR writes into aligned 32-bit writes to avoid assertions in the canonical
///   implementation.
/// - Ignores invalid sizes/offsets instead of panicking.
pub struct PciConfigSpaceCompat(RefCell<PciConfigSpace>);

impl PciConfigSpaceCompat {
    pub fn new(config: PciConfigSpace) -> Self {
        Self(RefCell::new(config))
    }

    fn validate_access(offset: u16, size: usize) -> bool {
        matches!(size, 1 | 2 | 4)
            && usize::from(offset)
                .checked_add(size)
                .is_some_and(|end| end <= PCI_CONFIG_SPACE_SIZE)
    }

    /// Reads a config-space value, returning a zero-extended `u32`.
    ///
    /// Invalid sizes/offsets return 0.
    pub fn read_u32(&self, offset: u16, size: usize) -> u32 {
        if !Self::validate_access(offset, size) {
            return 0;
        }

        let Ok(mut cfg) = self.0.try_borrow_mut() else {
            return 0;
        };
        cfg.read(offset, size)
    }

    /// Writes a config-space value.
    ///
    /// - Invalid sizes/offsets are ignored.
    /// - Subword/unaligned BAR writes are converted into an aligned 32-bit read-modify-write.
    pub fn write_u32(&self, offset: u16, size: usize, value: u32) {
        if !Self::validate_access(offset, size) {
            return;
        }

        let Ok(mut cfg) = self.0.try_borrow_mut() else {
            return;
        };

        if is_pci_bar_offset(offset) && (offset & 0x3 != 0 || size != 4) {
            let aligned = align_pci_dword(offset);

            // Defensively validate the aligned dword too, even though it should always be in range
            // for valid BAR offsets.
            if !Self::validate_access(aligned, 4) {
                return;
            }

            let old = cfg.read(aligned, 4);
            let shift = u32::from(offset - aligned) * 8;
            let mask = match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => return,
            };
            let merged = (old & !(mask << shift)) | ((value & mask) << shift);
            cfg.write(aligned, 4, merged);
            return;
        }

        cfg.write(offset, size, value);
    }
}

#[cfg(test)]
mod tests {
    use super::PciConfigSpaceCompat;
    use aero_devices::pci::{PciBarDefinition, PciConfigSpace};

    #[test]
    fn subword_bar_write_is_read_modify_write() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x10,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // Write 2 bytes starting at offset 0x11 (unaligned/subword). This must not panic.
        compat.write_u32(0x11, 2, 0xABCD);

        // The BAR dword should now read back with the correct little-endian byte placement.
        assert_eq!(compat.read_u32(0x10, 4), 0x00AB_CD00);
        assert_eq!(compat.read_u32(0x10, 1), 0x00);
        assert_eq!(compat.read_u32(0x11, 1), 0xCD);
        assert_eq!(compat.read_u32(0x12, 1), 0xAB);
        assert_eq!(compat.read_u32(0x13, 1), 0x00);
    }

    #[test]
    fn bar_size_probe_works_for_full_dword_writes() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // Standard BAR size probe: write all-ones dword and read back the size mask.
        compat.write_u32(0x10, 4, 0xFFFF_FFFF);
        assert_eq!(compat.read_u32(0x10, 4), 0xFFFF_F000);
    }
}

