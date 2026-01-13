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
use aero_devices::pci::{PciBarDefinition, PciConfigSpace};
use std::cell::RefCell;

const PCI_BAR_OFF_START: u16 = 0x10;
const PCI_BAR_OFF_END: u16 = 0x27;

#[inline]
fn is_pci_bar_offset(offset: u16) -> bool {
    (PCI_BAR_OFF_START..=PCI_BAR_OFF_END).contains(&offset)
}

#[inline]
fn access_overlaps_pci_bar(offset: u16, size: usize) -> bool {
    if size == 0 {
        return false;
    }
    // `validate_access` ensures `offset + size <= 256`, so this can't overflow.
    let end = offset + (size as u16).saturating_sub(1);
    offset <= PCI_BAR_OFF_END && end >= PCI_BAR_OFF_START
}

#[inline]
fn align_pci_dword(offset: u16) -> u16 {
    offset & !0x3
}

#[inline]
fn pci_bar_index(aligned_offset: u16) -> Option<u8> {
    if !is_pci_bar_offset(aligned_offset) || (aligned_offset & 0x3) != 0 {
        return None;
    }
    Some(((aligned_offset - PCI_BAR_OFF_START) / 4) as u8)
}

fn read_bar_dword_programmed(cfg: &mut PciConfigSpace, aligned_offset: u16) -> u32 {
    let Some(bar_index) = pci_bar_index(aligned_offset) else {
        return 0;
    };

    // When a BAR is being probed, canonical `PciConfigSpace::read` returns the probe mask rather
    // than the programmed base. For subword BAR writes we want to merge against the programmed
    // base, matching real hardware.
    if let Some(def) = cfg.bar_definition(bar_index) {
        let base = cfg.bar_range(bar_index).map(|r| r.base).unwrap_or(0);
        return match def {
            PciBarDefinition::Io { .. } => (base as u32 & 0xFFFF_FFFC) | 0x1,
            PciBarDefinition::Mmio32 { prefetchable, .. } => {
                let mut val = base as u32 & 0xFFFF_FFF0;
                if prefetchable {
                    val |= 1 << 3;
                }
                val
            }
            PciBarDefinition::Mmio64 { prefetchable, .. } => {
                let mut val = base as u32 & 0xFFFF_FFF0;
                val |= 0b10 << 1;
                if prefetchable {
                    val |= 1 << 3;
                }
                val
            }
        };
    }

    // High dword of a 64-bit BAR.
    if bar_index > 0 && matches!(cfg.bar_definition(bar_index - 1), Some(PciBarDefinition::Mmio64 { .. }))
    {
        let base = cfg.bar_range(bar_index - 1).map(|r| r.base).unwrap_or(0);
        return (base >> 32) as u32;
    }

    // Unknown BAR definition: defer to the canonical config bytes (not affected by BAR probe).
    cfg.read(aligned_offset, 4)
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

        // Canonical config-space reads only apply BAR special handling when the access begins
        // inside the BAR window. If a hostile guest performs a cross-dword read that partially
        // overlaps the BAR registers, read byte-by-byte so BAR bytes are still sourced from the
        // BAR state machine rather than the raw config bytes.
        if access_overlaps_pci_bar(offset, size) && !is_pci_bar_offset(offset) {
            let mut out = 0u32;
            for i in 0..size {
                let byte_off = offset + i as u16;
                let byte = cfg.read(byte_off, 1) & 0xFF;
                out |= byte << (8 * i);
            }
            return out;
        }

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

        // Canonical config-space writes only treat an access as a BAR write if the access begins
        // inside the BAR window; cross-dword accesses can therefore partially clobber BAR bytes
        // without updating BAR state. Handle any access that overlaps BAR registers (except for
        // the canonical aligned dword BAR write) via byte-splitting.
        let is_canonical_bar_dword = is_pci_bar_offset(offset) && (offset & 0x3) == 0 && size == 4;
        if access_overlaps_pci_bar(offset, size) && !is_canonical_bar_dword {
            // Perform a byte-granular write split across dword boundaries, issuing canonical
            // 32-bit writes for any bytes that land in the BAR window. This avoids panics from the
            // canonical BAR alignment assertions while still behaving like real hardware for
            // hostile/unaligned config accesses.
            let bytes = value.to_le_bytes();
            for i in 0..size {
                let byte_off = offset.wrapping_add(i as u16);
                let byte_val = u32::from(bytes[i]);

                if !Self::validate_access(byte_off, 1) {
                    continue;
                }

                if is_pci_bar_offset(byte_off) {
                    let aligned = align_pci_dword(byte_off);
                    if !Self::validate_access(aligned, 4) {
                        continue;
                    }

                    let old = read_bar_dword_programmed(&mut cfg, aligned);
                    let shift = u32::from(byte_off - aligned) * 8;
                    let merged = (old & !(0xFF << shift)) | (byte_val << shift);
                    cfg.write(aligned, 4, merged);
                } else {
                    // The original access started in the BAR window but can still spill into the
                    // next dword outside the BAR registers (e.g. offset=0x27 size=2). For those
                    // bytes, fall back to a normal byte write.
                    cfg.write(byte_off, 1, byte_val);
                }
            }
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

    #[test]
    fn bar_subword_writes_after_probe_use_programmed_base_not_probe_mask() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // BAR size probe returns the size mask, but does not overwrite the programmed base.
        compat.write_u32(0x10, 4, 0xFFFF_FFFF);
        assert_eq!(compat.read_u32(0x10, 4), 0xFFFF_F000);

        // Program only the high 16 bits via a subword write. The low 16 bits must remain from the
        // programmed base (0), not from the probe response (0xFFFF_F000).
        compat.write_u32(0x12, 2, 0xE000);
        assert_eq!(compat.read_u32(0x10, 4), 0xE000_0000);
    }

    #[test]
    fn unaligned_bar_write_that_crosses_dword_boundary_splits_cleanly() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x10,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // 16-bit write at 0x13 crosses BAR0 (0x10..0x13) into BAR1 low byte at 0x14.
        compat.write_u32(0x13, 2, 0xABCD);
        assert_eq!(compat.read_u32(0x10, 4), 0xCD00_0000);
        assert_eq!(compat.read_u32(0x14, 4), 0x0000_00AB);
    }

    #[test]
    fn config_write_that_starts_before_bar_and_spills_into_bar_updates_bar_state() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x10,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // Write a 16-bit value at 0x0F. This touches config byte 0x0F and BAR0 byte 0x10.
        compat.write_u32(0x0F, 2, 0xAA55);

        // BAR0 low byte should be updated via BAR state machine (with address bits masked to BAR
        // alignment). Only bits 7:4 can survive for a 16-byte aligned BAR.
        assert_eq!(compat.read_u32(0x10, 4), 0x0000_00A0);
    }
}
