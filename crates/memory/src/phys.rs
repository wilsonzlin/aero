use core::fmt;

/// Errors returned by [`GuestMemory`] backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestMemoryError {
    /// The requested address range is outside the guest physical memory size.
    OutOfRange { paddr: u64, len: usize, size: u64 },
    /// The requested size cannot be represented by the current platform's `usize`.
    SizeTooLarge { size: u64 },
    /// The chosen chunk size is invalid (e.g. zero).
    InvalidChunkSize { chunk_size: usize },
}

impl fmt::Display for GuestMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuestMemoryError::OutOfRange { paddr, len, size } => write!(
                f,
                "guest memory access out of range: paddr=0x{paddr:x} len={len} size=0x{size:x}"
            ),
            GuestMemoryError::SizeTooLarge { size } => {
                write!(f, "guest memory size {size} does not fit in usize")
            }
            GuestMemoryError::InvalidChunkSize { chunk_size } => {
                write!(f, "invalid guest memory chunk size {chunk_size}")
            }
        }
    }
}

impl std::error::Error for GuestMemoryError {}

pub type GuestMemoryResult<T> = Result<T, GuestMemoryError>;

/// Guest *physical* memory storage.
///
/// All externally-visible addresses are `u64` to support multi-GB WASM address spaces even on
/// `wasm32` where `usize` is 32-bit.
pub trait GuestMemory {
    fn size(&self) -> u64;

    /// Reads bytes from guest physical memory into `dst`.
    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()>;

    /// Writes bytes from `src` into guest physical memory.
    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()>;

    /// Optional fast-path: returns a contiguous slice if the backing storage is contiguous and
    /// allocated for the requested range.
    fn get_slice(&self, _paddr: u64, _len: usize) -> Option<&[u8]> {
        None
    }

    /// Optional fast-path: returns a contiguous mutable slice if the backing storage is contiguous
    /// and allocated for the requested range.
    fn get_slice_mut(&mut self, _paddr: u64, _len: usize) -> Option<&mut [u8]> {
        None
    }

    fn read_u8_le(&self, paddr: u64) -> GuestMemoryResult<u8> {
        let mut buf = [0u8; 1];
        self.read_into(paddr, &mut buf)?;
        Ok(buf[0])
    }

    fn read_u16_le(&self, paddr: u64) -> GuestMemoryResult<u16> {
        let mut buf = [0u8; 2];
        self.read_into(paddr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32_le(&self, paddr: u64) -> GuestMemoryResult<u32> {
        let mut buf = [0u8; 4];
        self.read_into(paddr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64_le(&self, paddr: u64) -> GuestMemoryResult<u64> {
        let mut buf = [0u8; 8];
        self.read_into(paddr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u128_le(&self, paddr: u64) -> GuestMemoryResult<u128> {
        let mut buf = [0u8; 16];
        self.read_into(paddr, &mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8_le(&mut self, paddr: u64, value: u8) -> GuestMemoryResult<()> {
        self.write_from(paddr, &[value])
    }

    fn write_u16_le(&mut self, paddr: u64, value: u16) -> GuestMemoryResult<()> {
        self.write_from(paddr, &value.to_le_bytes())
    }

    fn write_u32_le(&mut self, paddr: u64, value: u32) -> GuestMemoryResult<()> {
        self.write_from(paddr, &value.to_le_bytes())
    }

    fn write_u64_le(&mut self, paddr: u64, value: u64) -> GuestMemoryResult<()> {
        self.write_from(paddr, &value.to_le_bytes())
    }

    fn write_u128_le(&mut self, paddr: u64, value: u128) -> GuestMemoryResult<()> {
        self.write_from(paddr, &value.to_le_bytes())
    }
}

fn check_range(size: u64, paddr: u64, len: usize) -> GuestMemoryResult<()> {
    let len_u64 = len as u64;
    let end = paddr
        .checked_add(len_u64)
        .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
    if end > size {
        return Err(GuestMemoryError::OutOfRange { paddr, len, size });
    }
    Ok(())
}

/// Dense (contiguous) guest memory.
#[derive(Debug, Clone)]
pub struct DenseMemory {
    data: Box<[u8]>,
}

impl DenseMemory {
    pub fn new(size: u64) -> GuestMemoryResult<Self> {
        let size_usize =
            usize::try_from(size).map_err(|_| GuestMemoryError::SizeTooLarge { size })?;
        Ok(Self {
            data: vec![0u8; size_usize].into_boxed_slice(),
        })
    }

    #[inline]
    fn range_to_usize(&self, paddr: u64, len: usize) -> GuestMemoryResult<(usize, usize)> {
        check_range(self.size(), paddr, len)?;
        let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
            paddr,
            len,
            size: self.size(),
        })?;
        let end = start.checked_add(len).ok_or(GuestMemoryError::OutOfRange {
            paddr,
            len,
            size: self.size(),
        })?;
        Ok((start, end))
    }
}

impl GuestMemory for DenseMemory {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        let (start, end) = self.range_to_usize(paddr, dst.len())?;
        dst.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        let (start, end) = self.range_to_usize(paddr, src.len())?;
        self.data[start..end].copy_from_slice(src);
        Ok(())
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        let (start, end) = self.range_to_usize(paddr, len).ok()?;
        Some(&self.data[start..end])
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        let (start, end) = self.range_to_usize(paddr, len).ok()?;
        Some(&mut self.data[start..end])
    }
}

/// Sparse guest memory backed by lazily-allocated fixed-size chunks.
#[derive(Debug, Clone)]
pub struct SparseMemory {
    size: u64,
    chunk_size: usize,
    chunks: Vec<Option<Box<[u8]>>>,
}

impl SparseMemory {
    pub const DEFAULT_CHUNK_SIZE: usize = 2 * 1024 * 1024;

    pub fn new(size: u64) -> GuestMemoryResult<Self> {
        Self::with_chunk_size(size, Self::DEFAULT_CHUNK_SIZE)
    }

    pub fn with_chunk_size(size: u64, chunk_size: usize) -> GuestMemoryResult<Self> {
        if chunk_size == 0 {
            return Err(GuestMemoryError::InvalidChunkSize { chunk_size });
        }
        let chunk_size_u64 = chunk_size as u64;
        let chunk_count_u64 = size
            .checked_add(chunk_size_u64 - 1)
            .ok_or(GuestMemoryError::SizeTooLarge { size })?
            / chunk_size_u64;
        let chunk_count = usize::try_from(chunk_count_u64)
            .map_err(|_| GuestMemoryError::SizeTooLarge { size })?;
        Ok(Self {
            size,
            chunk_size,
            chunks: vec![None; chunk_count],
        })
    }

    #[inline]
    fn chunk_index(&self, paddr: u64) -> GuestMemoryResult<(usize, usize)> {
        let chunk_size_u64 = self.chunk_size as u64;
        let chunk = paddr / chunk_size_u64;
        let offset = paddr - chunk * chunk_size_u64;
        let chunk_usize = usize::try_from(chunk).map_err(|_| GuestMemoryError::OutOfRange {
            paddr,
            len: 1,
            size: self.size,
        })?;
        let offset_usize = usize::try_from(offset).expect("offset < chunk_size <= usize::MAX");
        Ok((chunk_usize, offset_usize))
    }

    #[inline]
    fn ensure_chunk(&mut self, chunk: usize) -> GuestMemoryResult<&mut [u8]> {
        let slot = self
            .chunks
            .get_mut(chunk)
            .ok_or(GuestMemoryError::OutOfRange {
                paddr: (chunk as u64) * (self.chunk_size as u64),
                len: 1,
                size: self.size,
            })?;
        if slot.is_none() {
            *slot = Some(vec![0u8; self.chunk_size].into_boxed_slice());
        }
        Ok(slot.as_mut().expect("just ensured"))
    }
}

impl GuestMemory for SparseMemory {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        check_range(self.size, paddr, dst.len())?;
        let mut remaining = dst;
        let mut cur = paddr;

        while !remaining.is_empty() {
            let (chunk_idx, chunk_off) = self.chunk_index(cur)?;
            let bytes_in_chunk = self.chunk_size - chunk_off;
            let take = bytes_in_chunk.min(remaining.len());

            if let Some(chunk) = self.chunks.get(chunk_idx).and_then(|c| c.as_ref()) {
                remaining[..take].copy_from_slice(&chunk[chunk_off..chunk_off + take]);
            } else {
                remaining[..take].fill(0);
            }

            cur += take as u64;
            remaining = &mut remaining[take..];

            // `cur` remains in range due to `check_range`.
            debug_assert!(cur <= self.size);
        }

        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        check_range(self.size, paddr, src.len())?;
        let mut remaining = src;
        let mut cur = paddr;

        while !remaining.is_empty() {
            let (chunk_idx, chunk_off) = self.chunk_index(cur)?;
            let bytes_in_chunk = self.chunk_size - chunk_off;
            let take = bytes_in_chunk.min(remaining.len());

            let chunk = self.ensure_chunk(chunk_idx)?;
            chunk[chunk_off..chunk_off + take].copy_from_slice(&remaining[..take]);

            cur += take as u64;
            remaining = &remaining[take..];
        }

        Ok(())
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        check_range(self.size, paddr, len).ok()?;
        let (chunk_idx, chunk_off) = self.chunk_index(paddr).ok()?;
        if chunk_off.checked_add(len)? > self.chunk_size {
            return None;
        }
        let chunk = self.chunks.get(chunk_idx)?.as_ref()?;
        Some(&chunk[chunk_off..chunk_off + len])
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        check_range(self.size, paddr, len).ok()?;
        let (chunk_idx, chunk_off) = self.chunk_index(paddr).ok()?;
        if chunk_off.checked_add(len)? > self.chunk_size {
            return None;
        }
        let chunk = self.chunks.get_mut(chunk_idx)?.as_mut()?;
        Some(&mut chunk[chunk_off..chunk_off + len])
    }
}

/// A guest memory wrapper that exposes a non-contiguous guest-physical address space by mapping
/// multiple guest ranges into a single backing [`GuestMemory`] implementation.
///
/// Any guest addresses not covered by a mapping behave as "open bus":
/// - reads return `0xFF`
/// - writes are ignored
///
/// This is useful for platforms with reserved MMIO holes in the physical address space (e.g. the
/// PC PCI hole below 4GiB) while still backing RAM with a contiguous allocation.
pub struct MappedGuestMemory {
    inner: Box<dyn GuestMemory>,
    regions: Vec<MappedRegionInternal>, // sorted by guest_start, disjoint
    size: u64,
}

impl fmt::Debug for MappedGuestMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedGuestMemory")
            .field("size", &format_args!("0x{:x}", self.size))
            .field("regions", &self.regions)
            .finish_non_exhaustive()
    }
}

/// A guest-physical mapping segment for [`MappedGuestMemory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedRegion {
    /// Start guest physical address of the mapped region.
    pub guest_start: u64,
    /// Start address within the backing memory exposed by the region.
    pub inner_start: u64,
    /// Length of the mapping in bytes.
    pub len: u64,
}

#[derive(Debug, Clone, Copy)]
struct MappedRegionInternal {
    guest_start: u64,
    guest_end: u64, // exclusive
    inner_start: u64,
}

impl MappedGuestMemory {
    /// Create a new mapped guest memory view over `inner`.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - any region has `len == 0`,
    /// - region bounds overflow `u64`,
    /// - regions overlap in guest address space,
    /// - or any region maps outside the backing memory size.
    pub fn new(inner: Box<dyn GuestMemory>, mut regions: Vec<MappedRegion>) -> Self {
        regions.sort_by_key(|r| r.guest_start);

        let inner_size = inner.size();
        let mut out_regions = Vec::with_capacity(regions.len());
        let mut last_guest_end = 0u64;
        let mut size = 0u64;

        for (idx, r) in regions.into_iter().enumerate() {
            assert!(r.len != 0, "mapped region length must be non-zero");

            let guest_end = r
                .guest_start
                .checked_add(r.len)
                .unwrap_or_else(|| panic!("mapped region guest_end overflow (idx={idx})"));
            let inner_end = r
                .inner_start
                .checked_add(r.len)
                .unwrap_or_else(|| panic!("mapped region inner_end overflow (idx={idx})"));

            if idx != 0 {
                assert!(
                    r.guest_start >= last_guest_end,
                    "mapped regions must be disjoint and sorted (idx={idx})"
                );
            }

            assert!(
                inner_end <= inner_size,
                "mapped region maps beyond backing size: inner_end=0x{inner_end:x} backing=0x{inner_size:x}"
            );

            last_guest_end = guest_end;
            size = size.max(guest_end);

            out_regions.push(MappedRegionInternal {
                guest_start: r.guest_start,
                guest_end,
                inner_start: r.inner_start,
            });
        }

        Self {
            inner,
            regions: out_regions,
            size,
        }
    }

    #[inline]
    fn find_region_index(&self, addr: u64) -> Option<usize> {
        let idx = self.regions.partition_point(|r| r.guest_start <= addr);
        let idx = idx.checked_sub(1)?;
        self.regions
            .get(idx)
            .is_some_and(|r| addr < r.guest_end)
            .then_some(idx)
    }

    #[inline]
    fn next_region_start_after(&self, addr: u64) -> Option<u64> {
        let idx = self.regions.partition_point(|r| r.guest_start <= addr);
        self.regions.get(idx).map(|r| r.guest_start)
    }
}

impl GuestMemory for MappedGuestMemory {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        check_range(self.size, paddr, dst.len())?;
        if dst.is_empty() {
            return Ok(());
        }

        let mut cur = paddr;
        let mut remaining = dst;
        while !remaining.is_empty() {
            if let Some(idx) = self.find_region_index(cur) {
                let region = self.regions[idx];
                let max_len = region.guest_end - cur;
                let take = max_len.min(remaining.len() as u64) as usize;

                let inner_paddr = region.inner_start + (cur - region.guest_start);
                self.inner.read_into(inner_paddr, &mut remaining[..take])?;

                cur += take as u64;
                remaining = &mut remaining[take..];
                continue;
            }

            // Open-bus hole: fill with 0xFF until the next mapped region (or end of address space).
            let next = self.next_region_start_after(cur).unwrap_or(self.size);
            let max_len = next.saturating_sub(cur);
            let take = max_len.min(remaining.len() as u64) as usize;
            remaining[..take].fill(0xFF);
            cur += take as u64;
            remaining = &mut remaining[take..];
        }

        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        check_range(self.size, paddr, src.len())?;
        if src.is_empty() {
            return Ok(());
        }

        let mut cur = paddr;
        let mut remaining = src;
        while !remaining.is_empty() {
            if let Some(idx) = self.find_region_index(cur) {
                let region = self.regions[idx];
                let max_len = region.guest_end - cur;
                let take = max_len.min(remaining.len() as u64) as usize;

                let inner_paddr = region.inner_start + (cur - region.guest_start);
                self.inner.write_from(inner_paddr, &remaining[..take])?;

                cur += take as u64;
                remaining = &remaining[take..];
                continue;
            }

            // Open-bus hole: ignore writes.
            let next = self.next_region_start_after(cur).unwrap_or(self.size);
            let max_len = next.saturating_sub(cur);
            let take = max_len.min(remaining.len() as u64) as usize;
            cur += take as u64;
            remaining = &remaining[take..];
        }

        Ok(())
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        check_range(self.size, paddr, len).ok()?;
        let idx = self.find_region_index(paddr)?;
        let region = self.regions[idx];

        let end = paddr.checked_add(len as u64)?;
        if end > region.guest_end {
            return None;
        }

        let inner_paddr = region.inner_start + (paddr - region.guest_start);
        self.inner.get_slice(inner_paddr, len)
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        check_range(self.size, paddr, len).ok()?;
        let idx = self.find_region_index(paddr)?;
        let region = self.regions[idx];

        let end = paddr.checked_add(len as u64)?;
        if end > region.guest_end {
            return None;
        }

        let inner_paddr = region.inner_start + (paddr - region.guest_start);
        self.inner.get_slice_mut(inner_paddr, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_read_write_primitives_aligned() {
        let mut mem = DenseMemory::new(64).unwrap();

        mem.write_u16_le(2, 0x1122).unwrap();
        mem.write_u32_le(4, 0x3344_5566).unwrap();
        mem.write_u64_le(8, 0x7788_99aa_bbcc_ddee).unwrap();
        mem.write_u128_le(16, 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10)
            .unwrap();

        assert_eq!(mem.read_u16_le(2).unwrap(), 0x1122);
        assert_eq!(mem.read_u32_le(4).unwrap(), 0x3344_5566);
        assert_eq!(mem.read_u64_le(8).unwrap(), 0x7788_99aa_bbcc_ddee);
        assert_eq!(
            mem.read_u128_le(16).unwrap(),
            0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10
        );
    }

    #[test]
    fn dense_read_write_primitives_unaligned() {
        let mut mem = DenseMemory::new(64).unwrap();

        mem.write_u32_le(1, 0xdead_beef).unwrap();
        mem.write_u64_le(9, 0x1122_3344_5566_7788).unwrap();

        assert_eq!(mem.read_u32_le(1).unwrap(), 0xdead_beef);
        assert_eq!(mem.read_u64_le(9).unwrap(), 0x1122_3344_5566_7788);
    }

    #[test]
    fn sparse_unallocated_reads_return_zero() {
        let mem = SparseMemory::with_chunk_size(128, 16).unwrap();
        assert_eq!(mem.read_u8_le(0).unwrap(), 0);
        assert_eq!(mem.read_u32_le(4).unwrap(), 0);
        assert_eq!(mem.read_u64_le(8).unwrap(), 0);
    }

    #[test]
    fn sparse_allocates_on_write() {
        let mut mem = SparseMemory::with_chunk_size(64, 16).unwrap();

        assert!(mem.chunks.iter().all(|c| c.is_none()));
        assert!(mem.get_slice(0, 4).is_none());

        mem.write_u8_le(3, 0xaa).unwrap();

        assert!(mem.chunks[0].is_some());
        assert_eq!(mem.read_u8_le(3).unwrap(), 0xaa);
        assert_eq!(mem.get_slice(0, 4).unwrap(), &[0, 0, 0, 0xaa]);
    }

    #[test]
    fn sparse_bulk_read_write_cross_chunk_boundary() {
        let mut mem = SparseMemory::with_chunk_size(64, 16).unwrap();

        // Write 8 bytes starting at offset 12: crosses from chunk 0 to chunk 1.
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
        mem.write_from(12, &src).unwrap();

        let mut dst = [0u8; 8];
        mem.read_into(12, &mut dst).unwrap();
        assert_eq!(dst, src);

        assert!(mem.chunks[0].is_some());
        assert!(mem.chunks[1].is_some());
    }

    #[test]
    fn sparse_primitive_cross_chunk_boundary() {
        let mut mem = SparseMemory::with_chunk_size(64, 16).unwrap();

        // u32 at address 14 crosses chunk boundary (14..18).
        mem.write_u32_le(14, 0x1122_3344).unwrap();
        assert_eq!(mem.read_u32_le(14).unwrap(), 0x1122_3344);
    }

    #[test]
    fn out_of_range_returns_error_without_panicking() {
        let mut dense = DenseMemory::new(16).unwrap();
        assert!(matches!(
            dense.read_u32_le(14),
            Err(GuestMemoryError::OutOfRange { .. })
        ));
        assert!(matches!(
            dense.write_u64_le(12, 0),
            Err(GuestMemoryError::OutOfRange { .. })
        ));

        let mut sparse = SparseMemory::with_chunk_size(16, 8).unwrap();
        assert!(matches!(
            sparse.read_into(15, &mut [0u8; 2]),
            Err(GuestMemoryError::OutOfRange { .. })
        ));
        assert!(matches!(
            sparse.write_from(16, &[1u8]),
            Err(GuestMemoryError::OutOfRange { .. })
        ));
    }
}
