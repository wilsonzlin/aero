//! Guest physical address translation helpers shared by wasm-side device DMA bridges.
//!
//! When guest RAM exceeds the PCIe ECAM base (0xB000_0000), the PC/Q35 layout remaps the "high"
//! portion of RAM above 4GiB, leaving a hole between ECAM and 4GiB:
//!
//! - Low RAM:  [0x0000_0000 .. 0xB000_0000)
//! - Hole:     [0xB000_0000 .. 0x1_0000_0000)  (ECAM + PCI MMIO aperture)
//! - High RAM: [0x1_0000_0000 .. 0x1_0000_0000 + (ram_bytes - 0xB000_0000))
//!
//! The wasm runtime stores guest RAM as a contiguous `[0..ram_bytes)` region in linear memory, so
//! devices that DMA into guest RAM must translate guest physical addresses back into this RAM
//! offset space.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuestRamRange {
    /// The entire requested range maps to guest RAM at the given offset.
    Ram { ram_offset: u64 },
    /// The entire requested range lies within the PCI/ECAM hole.
    Hole,
    /// The requested range is not backed by guest RAM (and is not in the hole).
    OutOfBounds,
}

/// Guest physical address where the PCIe ECAM window begins (and low RAM ends).
pub(crate) const PCIE_ECAM_BASE: u64 = aero_pc_constants::PCIE_ECAM_BASE;

/// Guest physical address where remapped high RAM begins.
pub(crate) const HIGH_RAM_BASE: u64 = 0x1_0000_0000;

/// Compute the physical address immediately following the last byte of guest RAM.
///
/// This is *not* the RAM byte size when high RAM remapping is active: the address space includes
/// the PCI/ECAM hole.
pub(crate) fn guest_ram_phys_end_exclusive(ram_bytes: u64) -> u64 {
    if ram_bytes <= PCIE_ECAM_BASE {
        ram_bytes
    } else {
        // Use saturating math so pathological `ram_bytes` inputs (e.g. fuzzing/tests that use
        // `u64::MAX`) cannot panic via u64 addition overflow.
        HIGH_RAM_BASE.saturating_add(ram_bytes - PCIE_ECAM_BASE)
    }
}

/// Translate a guest physical address range into a guest-RAM byte offset, accounting for the
/// ECAM/PCI hole and high-RAM remapping.
pub(crate) fn translate_guest_paddr_range(ram_bytes: u64, paddr: u64, len: usize) -> GuestRamRange {
    if len == 0 {
        // For zero-length accesses, return the containing region classification without requiring
        // the caller to special-case edge addresses.
        //
        // Note: the returned `ram_offset` is meaningful for callers that want to treat
        // `paddr == end` as a valid empty slice (mirrors slice indexing rules).
        if (PCIE_ECAM_BASE..HIGH_RAM_BASE).contains(&paddr) {
            return GuestRamRange::Hole;
        }

        let low_ram_bytes = ram_bytes.min(PCIE_ECAM_BASE);
        if paddr <= low_ram_bytes {
            return GuestRamRange::Ram { ram_offset: paddr };
        }

        let high_ram_bytes = ram_bytes.saturating_sub(PCIE_ECAM_BASE);
        if high_ram_bytes != 0 {
            let high_end = HIGH_RAM_BASE.saturating_add(high_ram_bytes);
            if (HIGH_RAM_BASE..=high_end).contains(&paddr) {
                let ram_offset = PCIE_ECAM_BASE + (paddr - HIGH_RAM_BASE);
                return GuestRamRange::Ram { ram_offset };
            }
        }

        return GuestRamRange::OutOfBounds;
    }

    let chunk = translate_guest_paddr_chunk(ram_bytes, paddr, len);
    match chunk {
        GuestRamChunk::Ram {
            ram_offset,
            len: chunk_len,
        } if chunk_len == len => GuestRamRange::Ram { ram_offset },
        GuestRamChunk::Hole { len: chunk_len } if chunk_len == len => GuestRamRange::Hole,
        _ => GuestRamRange::OutOfBounds,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuestRamChunk {
    Ram { ram_offset: u64, len: usize },
    Hole { len: usize },
    OutOfBounds { len: usize },
}

/// Split a DMA access into the first contiguous chunk that shares a single translation kind.
///
/// This is useful for byte-copy interfaces (HDA/UHCI/etc.) that can handle ranges spanning the RAM
/// hole by iterating.
pub(crate) fn translate_guest_paddr_chunk(ram_bytes: u64, paddr: u64, len: usize) -> GuestRamChunk {
    if len == 0 {
        return GuestRamChunk::OutOfBounds { len: 0 };
    }

    // Low RAM is capped at the ECAM base so the ECAM window never overlaps RAM.
    let low_ram_bytes = ram_bytes.min(PCIE_ECAM_BASE);

    // High RAM size is any remaining bytes above the low-RAM cap.
    let high_ram_bytes = ram_bytes.saturating_sub(PCIE_ECAM_BASE);
    let high_ram_end = HIGH_RAM_BASE.saturating_add(high_ram_bytes);

    let (kind, max_len_u64) = if paddr < low_ram_bytes {
        (
            GuestRamChunk::Ram {
                ram_offset: paddr,
                len: 0, // patched below
            },
            low_ram_bytes - paddr,
        )
    } else if paddr < PCIE_ECAM_BASE {
        // Address is below ECAM but past the end of low RAM (only possible when `ram_bytes` is
        // smaller than `PCIE_ECAM_BASE`).
        (
            GuestRamChunk::OutOfBounds { len: 0 },
            PCIE_ECAM_BASE - paddr,
        )
    } else if paddr < HIGH_RAM_BASE {
        (GuestRamChunk::Hole { len: 0 }, HIGH_RAM_BASE - paddr)
    } else if high_ram_bytes != 0 && paddr < high_ram_end {
        let ram_offset = PCIE_ECAM_BASE + (paddr - HIGH_RAM_BASE);
        (
            GuestRamChunk::Ram {
                ram_offset,
                len: 0, // patched below
            },
            high_ram_end - paddr,
        )
    } else {
        // Past the end of high RAM (or high RAM not present).
        (GuestRamChunk::OutOfBounds { len: 0 }, len as u64)
    };

    let max_len = (len as u64).min(max_len_u64);
    let max_len = max_len.min(usize::MAX as u64) as usize;

    match kind {
        GuestRamChunk::Ram { ram_offset, .. } => GuestRamChunk::Ram {
            ram_offset,
            len: max_len,
        },
        GuestRamChunk::Hole { .. } => GuestRamChunk::Hole { len: max_len },
        GuestRamChunk::OutOfBounds { .. } => GuestRamChunk::OutOfBounds { len: max_len },
    }
}
