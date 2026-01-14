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
fn validate_access(offset: u16, size: usize) -> bool {
    matches!(size, 1 | 2 | 4)
        && usize::from(offset)
            .checked_add(size)
            .is_some_and(|end| end <= PCI_CONFIG_SPACE_SIZE)
}

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

/// Read from a canonical [`PciConfigSpace`] using the emulator's forgiving semantics.
///
/// This performs bounds checks (to avoid panics) and applies special handling for reads that
/// partially overlap the BAR register block.
pub fn config_read(cfg: &mut PciConfigSpace, offset: u16, size: usize) -> u32 {
    if !validate_access(offset, size) {
        return 0;
    }

    // Canonical config-space reads only apply BAR special handling when the access begins inside
    // the BAR window and does not cross a dword boundary. If a hostile guest performs an
    // unaligned/cross-dword access that overlaps BAR registers, read byte-by-byte so:
    // - BAR bytes are still sourced from the BAR state machine (incl. probe masks), and
    // - reads that span two BAR dwords return bytes from both dwords, rather than truncating.
    if access_overlaps_pci_bar(offset, size) {
        let crosses_dword = (offset & 0x3) as usize + size > 4;
        if crosses_dword || !is_pci_bar_offset(offset) {
            let mut out = 0u32;
            for i in 0..size {
                let byte_off = offset + i as u16;
                let byte = cfg.read(byte_off, 1) & 0xFF;
                out |= byte << (8 * i);
            }
            return out;
        }
    }

    cfg.read(offset, size)
}

pub fn config_write(cfg: &mut PciConfigSpace, offset: u16, size: usize, value: u32) {
    if !validate_access(offset, size) {
        return;
    }

    // Canonical config-space writes only treat an access as a BAR write if the access begins
    // inside the BAR window; cross-dword accesses can therefore partially clobber BAR bytes
    // without updating BAR state. Handle any access that overlaps BAR registers (except for
    // the canonical aligned dword BAR write) via byte-splitting.
    let is_canonical_bar_dword = is_pci_bar_offset(offset) && (offset & 0x3) == 0 && size == 4;
    if access_overlaps_pci_bar(offset, size) && !is_canonical_bar_dword {
        // Perform a byte-granular write split across dword boundaries, issuing canonical 32-bit
        // writes for any bytes that land in the BAR window. This avoids panics from the canonical
        // BAR alignment assertions while still behaving like real hardware for hostile/unaligned
        // config accesses.
        let bytes = value.to_le_bytes();
        for i in 0..size {
            let byte_off = offset.wrapping_add(i as u16);
            let byte_val = u32::from(bytes[i]);

            if !validate_access(byte_off, 1) {
                continue;
            }

            if is_pci_bar_offset(byte_off) {
                let aligned = align_pci_dword(byte_off);
                if !validate_access(aligned, 4) {
                    continue;
                }

                let old = read_bar_dword_programmed(cfg, aligned);
                let shift = u32::from(byte_off - aligned) * 8;
                let merged = (old & !(0xFF << shift)) | (byte_val << shift);
                cfg.write(aligned, 4, merged);
            } else {
                // The original access started in the BAR window but can still spill into the next
                // dword outside the BAR registers (e.g. offset=0x27 size=2). For those bytes, fall
                // back to a normal byte write.
                cfg.write(byte_off, 1, byte_val);
            }
        }
        return;
    }

    cfg.write(offset, size, value);
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

    /// Runs `f` with a temporary mutable borrow of the underlying canonical config space.
    ///
    /// Returns `None` if the config space is already borrowed (e.g. re-entrant access).
    pub fn with_config_mut<R>(&self, f: impl FnOnce(&mut PciConfigSpace) -> R) -> Option<R> {
        self.0.try_borrow_mut().ok().map(|mut cfg| f(&mut cfg))
    }

    /// Runs `f` with a temporary immutable borrow of the underlying canonical config space.
    ///
    /// Returns `None` if the config space is already mutably borrowed.
    pub fn with_config<R>(&self, f: impl FnOnce(&PciConfigSpace) -> R) -> Option<R> {
        self.0.try_borrow().ok().map(|cfg| f(&cfg))
    }

    /// Consumes this wrapper and returns the underlying canonical config space.
    pub fn into_inner(self) -> PciConfigSpace {
        self.0.into_inner()
    }

    /// Reads a config-space value, returning a zero-extended `u32`.
    ///
    /// Invalid sizes/offsets return 0.
    pub fn read_u32(&self, offset: u16, size: usize) -> u32 {
        let Ok(mut cfg) = self.0.try_borrow_mut() else {
            return 0;
        };
        config_read(&mut cfg, offset, size)
    }

    /// Writes a config-space value.
    ///
    /// - Invalid sizes/offsets are ignored.
    /// - Subword/unaligned BAR writes are converted into an aligned 32-bit read-modify-write.
    pub fn write_u32(&self, offset: u16, size: usize, value: u32) {
        let Ok(mut cfg) = self.0.try_borrow_mut() else {
            return;
        };
        config_write(&mut cfg, offset, size, value);
    }
}

impl From<PciConfigSpace> for PciConfigSpaceCompat {
    fn from(value: PciConfigSpace) -> Self {
        Self::new(value)
    }
}

impl crate::io::pci::PciDevice for PciConfigSpaceCompat {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        self.read_u32(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        self.write_u32(offset, size, value);
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

    #[test]
    fn config_read_that_starts_before_bar_and_spills_into_bar_uses_bar_semantics() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // BAR probe: config bytes at 0x10..0x13 remain zero, but BAR reads return the size mask.
        compat.write_u32(0x10, 4, 0xFFFF_FFFF);
        assert_eq!(compat.read_u32(0x10, 4), 0xFFFF_F000);

        // Unaligned dword read starting at 0x0E overlaps BAR0 bytes 0x10..0x11. The helper must
        // source those bytes via BAR semantics, not the raw config bytes (which are still zero).
        //
        // Bytes at 0x10..0x13 for the size mask 0xFFFF_F000 are [00, F0, FF, FF].
        // Reading 4 bytes starting at 0x0E includes byte 0x11 as the top byte of the result.
        assert_eq!(compat.read_u32(0x0E, 4), 0xF000_0000);
    }

    #[test]
    fn bar_read_that_crosses_dword_boundary_reads_both_dwords() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x10,
                prefetchable: false,
            },
        );
        cfg.set_bar_definition(
            1,
            PciBarDefinition::Mmio32 {
                size: 0x10,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        compat.write_u32(0x10, 4, 0x1122_3340);
        compat.write_u32(0x14, 4, 0x5566_7780);

        // 16-bit read at 0x13 spans BAR0 byte3 (0x11) and BAR1 byte0 (0x80).
        assert_eq!(compat.read_u32(0x13, 2), 0x8011);
    }

    #[test]
    fn mmio64_bar_high_dword_subword_write_updates_base_without_panic() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: 0x4000,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // Program a low 32-bit base (flags are ignored by the canonical implementation and will
        // be reinserted on reads).
        compat.write_u32(0x10, 4, 0x2345_6000);
        assert_eq!(compat.read_u32(0x10, 4), 0x2345_4004);

        // Subword write into the high dword (BAR1). This must not panic and must update the high
        // dword as observed through config reads.
        compat.write_u32(0x15, 1, 0x01);
        assert_eq!(compat.read_u32(0x14, 4), 0x0000_0100);
        assert_eq!(compat.read_u32(0x10, 4), 0x2345_4004);
    }

    #[test]
    fn mmio64_bar_probe_then_subword_write_uses_programmed_base_not_probe_mask() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: 0x4000,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // Standard BAR size probe: write all-ones dword and read back the size mask (including the
        // "64-bit BAR" type bits 2:1 = 0b10).
        compat.write_u32(0x10, 4, 0xFFFF_FFFF);
        assert_eq!(compat.read_u32(0x10, 4), 0xFFFF_C004);

        // After probing, subword writes must merge against the programmed base (0), not the probe
        // response (0xFFFF_C004). Program only the high 16 bits of the low dword.
        compat.write_u32(0x12, 2, 0xE000);
        assert_eq!(compat.read_u32(0x10, 4), 0xE000_0004);
        assert_eq!(compat.read_u32(0x14, 4), 0x0000_0000);
    }

    #[test]
    fn io_bar_probe_and_subword_write_use_programmed_base_not_probe_mask() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(0, PciBarDefinition::Io { size: 0x20 });

        let compat = PciConfigSpaceCompat::new(cfg);

        compat.write_u32(0x10, 4, 0xFFFF_FFFF);
        assert_eq!(compat.read_u32(0x10, 4), 0xFFFF_FFE1);

        // Program only the high 16 bits via a subword write. Low bits must come from the
        // programmed base (0) + IO bit (bit0=1), not from the probe response (0xFFE1).
        compat.write_u32(0x12, 2, 0xE000);
        assert_eq!(compat.read_u32(0x10, 4), 0xE000_0001);
    }

    #[test]
    fn bar_write_that_spans_end_of_bar_window_updates_bar_and_next_reg() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            5,
            PciBarDefinition::Mmio32 {
                size: 0x10,
                prefetchable: false,
            },
        );

        let compat = PciConfigSpaceCompat::new(cfg);

        // 16-bit write at 0x27 touches BAR5 byte3 and config byte 0x28 (Cardbus CIS pointer low).
        compat.write_u32(0x27, 2, 0xABCD);

        assert_eq!(compat.read_u32(0x24, 4), 0xCD00_0000);
        assert_eq!(compat.read_u32(0x28, 1), 0xAB);
        assert_eq!(compat.read_u32(0x27, 2), 0xABCD);
    }
}
