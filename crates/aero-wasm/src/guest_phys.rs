//! Guest physical address translation helpers shared by wasm-side device DMA bridges.
//!
//! This module is a thin re-export of the shared [`aero_guest_phys`] implementation so the
//! translation logic stays consistent across WASM-side consumers (CPU worker, GPU worker, device
//! bridges).
//!
//! When guest RAM exceeds the PCIe ECAM base ([`PCIE_ECAM_BASE`]), the PC/Q35 layout remaps the
//! "high" portion of RAM above 4GiB, leaving a hole between ECAM and 4GiB:
//!
//! - Low RAM:  `[0x0000_0000 .. PCIE_ECAM_BASE)`
//! - Hole:     `[PCIE_ECAM_BASE .. 0x1_0000_0000)` (ECAM + PCI/MMIO hole)
//! - High RAM: `[0x1_0000_0000 .. 0x1_0000_0000 + (ram_bytes - PCIE_ECAM_BASE))`

#[allow(unused_imports)]
pub(crate) use aero_guest_phys::{
    GuestRamChunk, GuestRamRange, guest_ram_phys_end_exclusive, translate_guest_paddr_chunk,
    translate_guest_paddr_range,
};

/// Compatibility alias used by older call sites/tests in this crate.
#[allow(dead_code)]
pub(crate) const HIGH_RAM_BASE: u64 = aero_guest_phys::HIGH_RAM_START;

// Keep the ECAM base constant available for tests/debug helpers, but not all builds need it.
#[allow(unused_imports)]
pub(crate) use aero_guest_phys::PCIE_ECAM_BASE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_ram_phys_end_exclusive_matches_q35_layout() {
        // No remap when RAM is <= ECAM base.
        assert_eq!(guest_ram_phys_end_exclusive(0), 0);
        assert_eq!(
            guest_ram_phys_end_exclusive(PCIE_ECAM_BASE - 1),
            PCIE_ECAM_BASE - 1
        );
        assert_eq!(guest_ram_phys_end_exclusive(PCIE_ECAM_BASE), PCIE_ECAM_BASE);

        // When RAM exceeds ECAM base, the remainder is remapped above 4GiB.
        assert_eq!(
            guest_ram_phys_end_exclusive(PCIE_ECAM_BASE + 0x2000),
            HIGH_RAM_BASE + 0x2000
        );
    }

    #[test]
    fn translate_guest_paddr_range_classifies_low_hole_and_high_ram() {
        let ram_bytes = PCIE_ECAM_BASE + 0x2000;

        // Low RAM is identity-mapped.
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, 0x1000, 4),
            GuestRamRange::Ram { ram_offset: 0x1000 }
        );
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, PCIE_ECAM_BASE - 4, 4),
            GuestRamRange::Ram {
                ram_offset: PCIE_ECAM_BASE - 4
            }
        );

        // The ECAM/PCI/MMIO hole is not backed by RAM.
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, PCIE_ECAM_BASE, 4),
            GuestRamRange::Hole
        );
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, HIGH_RAM_BASE - 4, 4),
            GuestRamRange::Hole
        );

        // High RAM is remapped above 4GiB: physical 4GiB corresponds to RAM offset PCIE_ECAM_BASE.
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, HIGH_RAM_BASE, 4),
            GuestRamRange::Ram {
                ram_offset: PCIE_ECAM_BASE
            }
        );
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, HIGH_RAM_BASE + 0x1FFC, 4),
            GuestRamRange::Ram {
                ram_offset: PCIE_ECAM_BASE + 0x1FFC
            }
        );

        // The range API rejects accesses that span multiple regions (low RAM -> hole).
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, PCIE_ECAM_BASE - 2, 4),
            GuestRamRange::OutOfBounds
        );

        // The range API also rejects accesses that span the hole -> high-RAM boundary.
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, HIGH_RAM_BASE - 2, 4),
            GuestRamRange::OutOfBounds
        );

        // Out of range beyond the end of high RAM.
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, HIGH_RAM_BASE + 0x2000, 1),
            GuestRamRange::OutOfBounds
        );
    }

    #[test]
    fn translate_guest_paddr_chunk_splits_on_region_boundaries() {
        let ram_bytes = PCIE_ECAM_BASE + 0x2000;

        // Crossing low RAM -> hole.
        assert_eq!(
            translate_guest_paddr_chunk(ram_bytes, PCIE_ECAM_BASE - 2, 8),
            GuestRamChunk::Ram {
                ram_offset: PCIE_ECAM_BASE - 2,
                len: 2
            }
        );

        // Hole reads are chunked up to the 4GiB boundary.
        assert_eq!(
            translate_guest_paddr_chunk(ram_bytes, PCIE_ECAM_BASE, 8),
            GuestRamChunk::Hole { len: 8 }
        );
        assert_eq!(
            translate_guest_paddr_chunk(ram_bytes, HIGH_RAM_BASE - 4, 8),
            GuestRamChunk::Hole { len: 4 }
        );

        // Crossing hole -> high RAM.
        assert_eq!(
            translate_guest_paddr_chunk(ram_bytes, HIGH_RAM_BASE, 8),
            GuestRamChunk::Ram {
                ram_offset: PCIE_ECAM_BASE,
                len: 8
            }
        );
        assert_eq!(
            translate_guest_paddr_chunk(ram_bytes, HIGH_RAM_BASE + 0x1FFC, 8),
            GuestRamChunk::Ram {
                ram_offset: PCIE_ECAM_BASE + 0x1FFC,
                len: 4
            }
        );
    }

    #[test]
    fn translate_guest_paddr_range_zero_length_accepts_boundary_addresses() {
        let ram_bytes = PCIE_ECAM_BASE + 0x2000;

        // Empty slice at the end of low RAM is valid.
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, PCIE_ECAM_BASE, 0),
            GuestRamRange::Ram {
                ram_offset: PCIE_ECAM_BASE
            }
        );

        // Empty slice at the end of small/contiguous RAM is valid.
        assert_eq!(
            translate_guest_paddr_range(0x2000, 0x2000, 0),
            GuestRamRange::Ram { ram_offset: 0x2000 }
        );

        // Empty slice at the end of high RAM is valid.
        assert_eq!(
            translate_guest_paddr_range(ram_bytes, HIGH_RAM_BASE + 0x2000, 0),
            GuestRamRange::Ram {
                ram_offset: ram_bytes
            }
        );
    }
}
