use crate::phys::{GuestMemory, GuestMemoryError, GuestMemoryResult};
use core::fmt;

/// A guest-physical → inner-memory mapping region.
///
/// The guest-physical address range `[phys_start, phys_end)` is mapped to the inner backend address
/// range `[inner_offset, inner_offset + (phys_end - phys_start))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestMemoryMapping {
    pub phys_start: u64,
    pub phys_end: u64,
    pub inner_offset: u64,
}

impl GuestMemoryMapping {
    #[inline]
    fn len(&self) -> u64 {
        self.phys_end.saturating_sub(self.phys_start)
    }
}

/// Errors constructing a [`MappedGuestMemory`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappedGuestMemoryError {
    /// A mapping has `phys_end <= phys_start`.
    EmptyRegion {
        index: usize,
        phys_start: u64,
        phys_end: u64,
    },
    /// A mapping lies (partially or fully) outside the declared guest-physical address space size.
    RegionOutOfPhysRange {
        index: usize,
        phys_start: u64,
        phys_end: u64,
        phys_size: u64,
    },
    /// Guest-physical mappings overlap after sorting by `phys_start`.
    Overlap {
        prev_index: usize,
        prev_end: u64,
        index: usize,
        phys_start: u64,
    },
    /// The mapped inner range lies outside the inner backend's size.
    InnerOutOfRange {
        index: usize,
        inner_offset: u64,
        len: u64,
        inner_size: u64,
    },
    /// The mapped inner range would overflow `u64`.
    InnerAddressOverflow {
        index: usize,
        inner_offset: u64,
        len: u64,
    },
}

impl fmt::Display for MappedGuestMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MappedGuestMemoryError::EmptyRegion {
                index,
                phys_start,
                phys_end,
            } => write!(
                f,
                "mapped guest memory region {index} is empty: phys_start=0x{phys_start:x} phys_end=0x{phys_end:x}"
            ),
            MappedGuestMemoryError::RegionOutOfPhysRange {
                index,
                phys_start,
                phys_end,
                phys_size,
            } => write!(
                f,
                "mapped guest memory region {index} out of phys range: phys_start=0x{phys_start:x} phys_end=0x{phys_end:x} phys_size=0x{phys_size:x}"
            ),
            MappedGuestMemoryError::Overlap {
                prev_index,
                prev_end,
                index,
                phys_start,
            } => write!(
                f,
                "mapped guest memory regions overlap: prev_index={prev_index} prev_end=0x{prev_end:x} index={index} phys_start=0x{phys_start:x}"
            ),
            MappedGuestMemoryError::InnerOutOfRange {
                index,
                inner_offset,
                len,
                inner_size,
            } => write!(
                f,
                "mapped guest memory region {index} out of inner range: inner_offset=0x{inner_offset:x} len=0x{len:x} inner_size=0x{inner_size:x}"
            ),
            MappedGuestMemoryError::InnerAddressOverflow {
                index,
                inner_offset,
                len,
            } => write!(
                f,
                "mapped guest memory region {index} inner range overflows: inner_offset=0x{inner_offset:x} len=0x{len:x}"
            ),
        }
    }
}

impl std::error::Error for MappedGuestMemoryError {}

/// A [`GuestMemory`] wrapper that exposes a guest-physical address space containing holes.
///
/// The guest sees an address space of size `phys_size` (`[0, phys_size)`), backed by one or more
/// non-overlapping mapped regions. Accesses into unmapped gaps succeed, returning `0xFF` on reads
/// (open-bus) and ignoring writes.
///
/// This is used to model PC-style memory layouts where RAM is remapped above 4GiB to make room for
/// MMIO windows, while the emulator keeps RAM in a contiguous backing store.
pub struct MappedGuestMemory {
    inner: Box<dyn GuestMemory>,
    phys_size: u64,
    regions: Vec<GuestMemoryMapping>,
}

impl MappedGuestMemory {
    /// Wrap `inner` and expose it via the guest-physical `regions` within `[0, phys_size)`.
    ///
    /// `regions` are sorted by `phys_start` and validated to be disjoint. Each region's translated
    /// inner range must lie within `inner.size()`.
    pub fn new(
        inner: Box<dyn GuestMemory>,
        phys_size: u64,
        mut regions: Vec<GuestMemoryMapping>,
    ) -> Result<Self, MappedGuestMemoryError> {
        regions.sort_by_key(|r| r.phys_start);

        let inner_size = inner.size();
        let mut prev_end = 0u64;

        for (idx, r) in regions.iter().enumerate() {
            if r.phys_end <= r.phys_start {
                return Err(MappedGuestMemoryError::EmptyRegion {
                    index: idx,
                    phys_start: r.phys_start,
                    phys_end: r.phys_end,
                });
            }
            if r.phys_end > phys_size {
                return Err(MappedGuestMemoryError::RegionOutOfPhysRange {
                    index: idx,
                    phys_start: r.phys_start,
                    phys_end: r.phys_end,
                    phys_size,
                });
            }

            if idx > 0 && r.phys_start < prev_end {
                return Err(MappedGuestMemoryError::Overlap {
                    prev_index: idx - 1,
                    prev_end,
                    index: idx,
                    phys_start: r.phys_start,
                });
            }

            let len = r.len();
            let inner_end = r.inner_offset.checked_add(len).ok_or(
                MappedGuestMemoryError::InnerAddressOverflow {
                    index: idx,
                    inner_offset: r.inner_offset,
                    len,
                },
            )?;
            if inner_end > inner_size {
                return Err(MappedGuestMemoryError::InnerOutOfRange {
                    index: idx,
                    inner_offset: r.inner_offset,
                    len,
                    inner_size,
                });
            }

            prev_end = r.phys_end;
        }

        Ok(Self {
            inner,
            phys_size,
            regions,
        })
    }

    pub fn phys_size(&self) -> u64 {
        self.phys_size
    }

    pub fn regions(&self) -> &[GuestMemoryMapping] {
        &self.regions
    }

    #[inline]
    fn check_range(&self, paddr: u64, len: usize) -> GuestMemoryResult<u64> {
        let end = paddr.checked_add(len as u64).ok_or(GuestMemoryError::OutOfRange {
            paddr,
            len,
            size: self.phys_size,
        })?;
        if end > self.phys_size {
            return Err(GuestMemoryError::OutOfRange {
                paddr,
                len,
                size: self.phys_size,
            });
        }
        Ok(end)
    }

    #[inline]
    fn first_region_index_for_addr(&self, paddr: u64) -> usize {
        // Safe because regions are sorted/disjoint, so `phys_end` is strictly increasing.
        self.regions.partition_point(|r| r.phys_end <= paddr)
    }

    #[inline]
    fn map_addr(region: &GuestMemoryMapping, paddr: u64) -> u64 {
        // Safe due to construction-time validation.
        region.inner_offset + (paddr - region.phys_start)
    }
}

impl GuestMemory for MappedGuestMemory {
    fn size(&self) -> u64 {
        self.phys_size
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        let end = self.check_range(paddr, dst.len())?;
        if dst.is_empty() {
            return Ok(());
        }

        // Open-bus default for holes.
        dst.fill(0xFF);

        let mut idx = self.first_region_index_for_addr(paddr);
        while let Some(region) = self.regions.get(idx) {
            if region.phys_start >= end {
                break;
            }

            let inter_start = paddr.max(region.phys_start);
            let inter_end = end.min(region.phys_end);
            if inter_start < inter_end {
                // `inter_*` are within `[paddr, end)` which is within `dst.len()`.
                let dst_off = (inter_start - paddr) as usize;
                let inter_len = (inter_end - inter_start) as usize;

                let inner_addr = Self::map_addr(region, inter_start);
                self.inner
                    .read_into(inner_addr, &mut dst[dst_off..dst_off + inter_len])?;
            }

            idx += 1;
        }

        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        let end = self.check_range(paddr, src.len())?;
        if src.is_empty() {
            return Ok(());
        }

        let mut idx = self.first_region_index_for_addr(paddr);
        while let Some(region) = self.regions.get(idx) {
            if region.phys_start >= end {
                break;
            }

            let inter_start = paddr.max(region.phys_start);
            let inter_end = end.min(region.phys_end);
            if inter_start < inter_end {
                let src_off = (inter_start - paddr) as usize;
                let inter_len = (inter_end - inter_start) as usize;

                let inner_addr = Self::map_addr(region, inter_start);
                self.inner
                    .write_from(inner_addr, &src[src_off..src_off + inter_len])?;
            }

            idx += 1;
        }

        Ok(())
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        let end = paddr.checked_add(len as u64)?;
        if end > self.phys_size {
            return None;
        }

        let idx = self.regions.partition_point(|r| r.phys_start <= paddr).checked_sub(1)?;
        let region = self.regions.get(idx)?;
        if paddr >= region.phys_end || end > region.phys_end {
            return None;
        }

        let inner_addr = Self::map_addr(region, paddr);
        self.inner.get_slice(inner_addr, len)
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        let end = paddr.checked_add(len as u64)?;
        if end > self.phys_size {
            return None;
        }

        let idx = self.regions.partition_point(|r| r.phys_start <= paddr).checked_sub(1)?;
        let region = self.regions.get(idx)?;
        if paddr >= region.phys_end || end > region.phys_end {
            return None;
        }

        let inner_addr = Self::map_addr(region, paddr);
        self.inner.get_slice_mut(inner_addr, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phys::{DenseMemory, SparseMemory};

    #[test]
    fn reads_and_writes_translate_and_holes_are_open_bus() {
        let mut inner = DenseMemory::new(0x200).unwrap();
        inner.get_slice_mut(0, 0x100).unwrap().fill(0xAA);
        inner.get_slice_mut(0x100, 0x100).unwrap().fill(0xBB);

        let mut mem = MappedGuestMemory::new(
            Box::new(inner),
            0x300,
            vec![
                GuestMemoryMapping {
                    phys_start: 0x000,
                    phys_end: 0x100,
                    inner_offset: 0x000,
                },
                GuestMemoryMapping {
                    phys_start: 0x200,
                    phys_end: 0x300,
                    inner_offset: 0x100,
                },
            ],
        )
        .unwrap();

        // Basic translation.
        let mut buf = [0u8; 4];
        mem.read_into(0x0, &mut buf).unwrap();
        assert_eq!(buf, [0xAA; 4]);

        mem.read_into(0x100, &mut buf).unwrap();
        assert_eq!(buf, [0xFF; 4]);

        mem.read_into(0x200, &mut buf).unwrap();
        assert_eq!(buf, [0xBB; 4]);

        // Straddling mapped -> hole -> mapped should succeed and include both mapped bytes and
        // open-bus 0xFF bytes.
        let mut got = vec![0u8; 0x140];
        mem.read_into(0x0F0, &mut got).unwrap();
        assert_eq!(&got[..0x10], &[0xAA; 0x10]);
        assert_eq!(&got[0x10..0x110], &[0xFF; 0x100]);
        assert_eq!(&got[0x110..], &[0xBB; 0x30]);

        // Writes entirely within holes are ignored but succeed.
        mem.write_from(0x180, &[1, 2, 3, 4]).unwrap();
        mem.read_into(0x180, &mut buf).unwrap();
        assert_eq!(buf, [0xFF; 4]);

        // Straddling writes should update only mapped bytes.
        let src: Vec<u8> = (0..0x140).map(|v| v as u8).collect();
        mem.write_from(0x0F0, &src).unwrap();

        let mut got = vec![0u8; 0x140];
        mem.read_into(0x0F0, &mut got).unwrap();

        let mut expected = vec![0xFF; 0x140];
        expected[..0x10].copy_from_slice(&src[..0x10]);
        expected[0x110..].copy_from_slice(&src[0x110..]);
        assert_eq!(got, expected);

        // Reads beyond phys_size should error.
        assert!(matches!(
            mem.read_into(0x2FF, &mut [0u8; 2]),
            Err(GuestMemoryError::OutOfRange { .. })
        ));
    }

    #[test]
    fn get_slice_fast_path_only_for_single_mapped_region() {
        let mut inner = DenseMemory::new(0x200).unwrap();
        inner.get_slice_mut(0, 0x100).unwrap().fill(0xAA);
        inner.get_slice_mut(0x100, 0x100).unwrap().fill(0xBB);

        let mut mem = MappedGuestMemory::new(
            Box::new(inner),
            0x300,
            vec![
                GuestMemoryMapping {
                    phys_start: 0x000,
                    phys_end: 0x100,
                    inner_offset: 0x000,
                },
                GuestMemoryMapping {
                    phys_start: 0x200,
                    phys_end: 0x300,
                    inner_offset: 0x100,
                },
            ],
        )
        .unwrap();

        assert_eq!(mem.get_slice(0x0, 4).unwrap(), &[0xAA; 4]);
        assert!(mem.get_slice(0x100, 4).is_none()); // hole
        assert!(mem.get_slice(0x0F0, 0x40).is_none()); // crosses into hole
        assert_eq!(mem.get_slice(0x200, 4).unwrap(), &[0xBB; 4]);

        let slice = mem.get_slice_mut(0x200, 4).unwrap();
        slice.copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(mem.get_slice(0x200, 4).unwrap(), &[1, 2, 3, 4]);
    }

    #[test]
    fn supports_large_phys_addrs_with_sparse_inner_without_allocating_gigabytes() {
        const PCIE_ECAM_BASE: u64 = 0xB000_0000;
        let total_memory = PCIE_ECAM_BASE + 0x2000;
        let phys_size = 0x1_0000_0000 + (total_memory - PCIE_ECAM_BASE);

        let inner = SparseMemory::with_chunk_size(total_memory, 2 * 1024 * 1024).unwrap();
        let mut mem = MappedGuestMemory::new(
            Box::new(inner),
            phys_size,
            vec![
                GuestMemoryMapping {
                    phys_start: 0x0,
                    phys_end: PCIE_ECAM_BASE,
                    inner_offset: 0x0,
                },
                GuestMemoryMapping {
                    phys_start: 0x1_0000_0000,
                    phys_end: phys_size,
                    inner_offset: PCIE_ECAM_BASE,
                },
            ],
        )
        .unwrap();

        // Verify the hole is open-bus.
        let mut hole = [0u8; 4];
        mem.read_into(PCIE_ECAM_BASE, &mut hole).unwrap();
        assert_eq!(hole, [0xFF; 4]);

        // Write 16 bytes at >= 4GiB and read back across the 4GiB boundary.
        let pattern: Vec<u8> = (0..16).map(|v| v as u8).collect();
        mem.write_from(0x1_0000_0000, &pattern).unwrap();

        let mut buf = vec![0u8; 32];
        mem.read_into(0xFFFF_FFF0, &mut buf).unwrap();
        assert_eq!(&buf[..16], &[0xFF; 16]);
        assert_eq!(&buf[16..], &pattern);
    }
}

