#![forbid(unsafe_code)]
#![no_std]

//! Guest physical address translation helpers for the PC/Q35 memory layout.
//!
//! The browser/WASM runtime stores guest RAM as a contiguous `[0..ram_bytes)` byte array in linear
//! memory. On PC/Q35, guest *physical* RAM becomes non-contiguous once configured guest RAM exceeds
//! the PCIe ECAM base ([`PCIE_ECAM_BASE`]): the portion of RAM above [`PCIE_ECAM_BASE`] is remapped
//! above 4GiB ([`HIGH_RAM_START`]), leaving an ECAM + PCI/MMIO hole below 4GiB:
//!
//! - Low RAM:  `[0x0000_0000 .. PCIE_ECAM_BASE)`
//! - Hole:     `[PCIE_ECAM_BASE .. 0x1_0000_0000)` (ECAM + PCI/MMIO hole)
//! - High RAM: `[0x1_0000_0000 .. 0x1_0000_0000 + (ram_bytes - PCIE_ECAM_BASE))`
//!
//! Devices that DMA into guest RAM must translate guest physical addresses back into offsets inside
//! the backing store.

/// Guest physical address where the PCIe ECAM window begins (and low RAM ends).
pub const PCIE_ECAM_BASE: u64 = aero_pc_constants::PCIE_ECAM_BASE;

/// Alias for [`PCIE_ECAM_BASE`] used by some consumers.
pub const LOW_RAM_END: u64 = PCIE_ECAM_BASE;

/// Guest physical address where remapped high RAM begins (4GiB).
pub const HIGH_RAM_START: u64 = 0x1_0000_0000;

/// Classification of a guest-physical address range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestRamRange {
    /// The entire requested range maps to guest RAM at the given offset.
    Ram { ram_offset: u64 },
    /// The entire requested range lies within the PCI/ECAM hole.
    Hole,
    /// The requested range is not backed by guest RAM (and is not fully in the hole).
    OutOfBounds,
}

/// Classification of the first contiguous chunk of a guest-physical range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestRamChunk {
    /// The chunk is backed by guest RAM at the given offset.
    Ram { ram_offset: u64, len: usize },
    /// The chunk lies within the PCI/ECAM hole.
    Hole { len: usize },
    /// The chunk is not backed by RAM (and is not in the hole).
    OutOfBounds { len: usize },
}

/// Compute the physical address immediately following the last byte of guest RAM.
///
/// This is *not* the RAM byte size when high RAM remapping is active: the address space includes
/// the PCI/ECAM/MMIO hole.
pub fn guest_ram_phys_end_exclusive(ram_bytes: u64) -> u64 {
    if ram_bytes <= LOW_RAM_END {
        ram_bytes
    } else {
        HIGH_RAM_START.saturating_add(ram_bytes.saturating_sub(LOW_RAM_END))
    }
}

/// Translate a guest physical address range into a guest-RAM byte offset, accounting for the
/// ECAM/PCI hole and high-RAM remapping.
pub fn translate_guest_paddr_range(ram_bytes: u64, paddr: u64, len: usize) -> GuestRamRange {
    if len == 0 {
        return translate_guest_paddr_empty_range(ram_bytes, paddr);
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

fn translate_guest_paddr_empty_range(ram_bytes: u64, paddr: u64) -> GuestRamRange {
    // Low RAM is capped at the ECAM base so the ECAM window never overlaps RAM.
    let low_ram_bytes = ram_bytes.min(LOW_RAM_END);
    if paddr <= low_ram_bytes {
        // Empty slice at the end of low RAM is valid.
        return GuestRamRange::Ram { ram_offset: paddr };
    }

    // The ECAM/PCI/MMIO hole is always present in the PC/Q35 physical address space, regardless of
    // the configured RAM size.
    if (LOW_RAM_END..HIGH_RAM_START).contains(&paddr) {
        return GuestRamRange::Hole;
    }

    // High RAM size is any remaining bytes above the low-RAM cap.
    let high_ram_bytes = ram_bytes.saturating_sub(LOW_RAM_END);
    if high_ram_bytes != 0 {
        let high_end = HIGH_RAM_START.saturating_add(high_ram_bytes);
        if paddr >= HIGH_RAM_START && paddr <= high_end {
            let diff = paddr - HIGH_RAM_START;
            let Some(ram_offset) = LOW_RAM_END.checked_add(diff) else {
                return GuestRamRange::OutOfBounds;
            };
            return GuestRamRange::Ram { ram_offset };
        }
    }

    GuestRamRange::OutOfBounds
}

/// Translate a guest physical range into the first contiguous chunk that shares a single
/// translation kind.
///
/// This is useful for byte-copy interfaces (HDA/UHCI/etc.) that can handle ranges spanning the RAM
/// hole by iterating.
pub fn translate_guest_paddr_chunk(ram_bytes: u64, paddr: u64, len: usize) -> GuestRamChunk {
    if len == 0 {
        // For consistency with `translate_guest_paddr_range`, treat empty ranges as valid at RAM
        // boundaries and within the ECAM/PCI hole.
        return match translate_guest_paddr_empty_range(ram_bytes, paddr) {
            GuestRamRange::Ram { ram_offset } => GuestRamChunk::Ram { ram_offset, len: 0 },
            GuestRamRange::Hole => GuestRamChunk::Hole { len: 0 },
            GuestRamRange::OutOfBounds => GuestRamChunk::OutOfBounds { len: 0 },
        };
    }

    // Low RAM is capped at the ECAM base so the ECAM window never overlaps RAM.
    let low_ram_bytes = ram_bytes.min(LOW_RAM_END);

    // High RAM size is any remaining bytes above the low-RAM cap.
    let high_ram_bytes = ram_bytes.saturating_sub(LOW_RAM_END);
    let high_ram_end = HIGH_RAM_START.saturating_add(high_ram_bytes);

    let (kind, max_len_u64) = if paddr < low_ram_bytes {
        (
            GuestRamChunk::Ram {
                ram_offset: paddr,
                len: 0, // patched below
            },
            low_ram_bytes - paddr,
        )
    } else if paddr < LOW_RAM_END {
        // Address is below ECAM but past the end of low RAM (only possible when `ram_bytes` is
        // smaller than `LOW_RAM_END`).
        (
            GuestRamChunk::OutOfBounds { len: 0 },
            LOW_RAM_END - paddr,
        )
    } else if paddr < HIGH_RAM_START {
        (
            GuestRamChunk::Hole { len: 0 },
            HIGH_RAM_START - paddr,
        )
    } else if high_ram_bytes != 0 && paddr < high_ram_end {
        let diff = paddr - HIGH_RAM_START;
        let Some(ram_offset) = LOW_RAM_END.checked_add(diff) else {
            // Should be unreachable with the current constants, but keep the function panic-free
            // for pathological fuzz inputs.
            return GuestRamChunk::OutOfBounds { len };
        };
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

/// Translate a guest physical address range into a backing-RAM offset.
///
/// Returns `Some(ram_offset)` if (and only if) the entire range is backed by RAM.
/// Returns `None` for hole/out-of-range ranges, or for ranges that span multiple regions.
pub fn translate_guest_paddr_range_to_offset(ram_bytes: u64, paddr: u64, len: u64) -> Option<u64> {
    let len = usize::try_from(len).ok()?;
    match translate_guest_paddr_range(ram_bytes, paddr, len) {
        GuestRamRange::Ram { ram_offset } => Some(ram_offset),
        GuestRamRange::Hole | GuestRamRange::OutOfBounds => None,
    }
}
