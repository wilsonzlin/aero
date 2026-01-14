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

// -------------------------------------------------------------------------------------------------
// wasm32 shared-linear-memory guest RAM backend
// -------------------------------------------------------------------------------------------------

/// A [`GuestMemory`] backend backed by a fixed region inside a wasm32 linear memory.
///
/// # Why `get_slice`/`get_slice_mut` always return `None`
/// In the browser runtime, guest RAM is backed by a *shared* [`WebAssembly.Memory`] (shared linear
/// memory) so that multiple wasm threads (and the JS host) can access the same RAM.
///
/// Returning `&[u8]` / `&mut [u8]` that points into this shared region is **unsound** under Rust's
/// aliasing model:
/// - `&mut [u8]` requires *unique* access for its lifetime. In shared wasm memory, other threads
///   (or JS) may concurrently read/write the same bytes, violating that uniqueness and causing UB.
/// - Even `&[u8]` is problematic: the compiler may assume the referenced bytes are not mutated for
///   the lifetime of the borrow, but shared wasm memory permits concurrent mutation, which again can
///   violate Rust's assumptions.
///
/// Therefore this backend deliberately disables the optional slice fast paths and forces all
/// callers to use the copy-based APIs (`read_into`/`write_from`), which do not create Rust
/// references into the shared backing store.
///
/// # Threading / data races
/// When compiled with `target_feature=atomics` (shared-memory wasm builds), this backend uses
/// byte-granular atomic loads/stores to avoid Rust UB from unsynchronized concurrent accesses.
///
/// In non-atomic wasm32 builds, guest RAM is not shared across threads, so plain memcpy-style access
/// is used.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Copy)]
pub struct WasmSharedGuestMemory {
    /// Base address (byte offset) in the process address space of guest physical address 0.
    ///
    /// On wasm32 this is the linear-memory offset (because wasm linear memory is mapped starting at
    /// 0). In unit tests we construct this from a raw pointer.
    base: usize,
    size: u64,
}

#[cfg(any(target_arch = "wasm32", test))]
impl WasmSharedGuestMemory {
    /// Create a new guest RAM view backed by wasm linear memory 0.
    ///
    /// `guest_base` is the byte offset in wasm linear memory corresponding to guest physical
    /// address 0. The guest RAM region spans `[guest_base, guest_base + size)`.
    ///
    /// This constructor validates that the region fits in the current linear memory.
    #[cfg(target_arch = "wasm32")]
    pub fn new(guest_base: u32, size: u64) -> GuestMemoryResult<Self> {
        let _size_usize =
            usize::try_from(size).map_err(|_| GuestMemoryError::SizeTooLarge { size })?;

        let mem_bytes = (core::arch::wasm32::memory_size(0) as u64).saturating_mul(64 * 1024);
        let base_u64 = u64::from(guest_base);
        let available = mem_bytes.saturating_sub(base_u64);
        if size > available {
            return Err(GuestMemoryError::OutOfRange {
                paddr: 0,
                len: size.min(usize::MAX as u64) as usize,
                size: available,
            });
        }

        Ok(Self {
            base: guest_base as usize,
            size,
        })
    }

    /// Create a guest RAM view from a raw pointer.
    ///
    /// This is intended for unit tests and other non-wasm environments.
    ///
    /// # Safety
    /// The caller must ensure:
    /// - `base` points to a valid allocation of at least `size` bytes for the lifetime of the
    ///   returned [`WasmSharedGuestMemory`].
    /// - All concurrent accesses to the region are properly synchronized *or* are performed via
    ///   atomic operations (e.g. in a `target_feature=atomics` wasm build).
    #[cfg(any(test, not(target_arch = "wasm32")))]
    pub unsafe fn from_raw_ptr(base: *mut u8, size: u64) -> GuestMemoryResult<Self> {
        let _size_usize =
            usize::try_from(size).map_err(|_| GuestMemoryError::SizeTooLarge { size })?;
        let base_usize = base as usize;
        base_usize
            .checked_add(size as usize)
            .ok_or(GuestMemoryError::SizeTooLarge { size })?;
        Ok(Self { base: base_usize, size })
    }

    #[inline]
    fn range_to_ptr(&self, paddr: u64, len: usize) -> GuestMemoryResult<usize> {
        check_range(self.size, paddr, len)?;
        let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
            paddr,
            len,
            size: self.size,
        })?;
        let ptr = self.base.checked_add(start).ok_or(GuestMemoryError::OutOfRange {
            paddr,
            len,
            size: self.size,
        })?;
        ptr.checked_add(len).ok_or(GuestMemoryError::OutOfRange {
            paddr,
            len,
            size: self.size,
        })?;
        Ok(ptr)
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl GuestMemory for WasmSharedGuestMemory {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        if dst.is_empty() {
            return Ok(());
        }
        let src = self.range_to_ptr(paddr, dst.len())?;

        // Shared-memory (atomics) builds: use atomic byte reads to avoid Rust data-race UB.
        #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
        {
            use core::sync::atomic::{AtomicU8, Ordering};

            let src = src as *const AtomicU8;
            for (i, slot) in dst.iter_mut().enumerate() {
                // Safety: `range_to_ptr` bounds-checks and `AtomicU8` has alignment 1.
                *slot = unsafe { (&*src.add(i)).load(Ordering::Relaxed) };
            }
        }

        // Non-atomic wasm builds: linear memory is not shared across threads, so memcpy is fine.
        #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
        unsafe {
            core::ptr::copy_nonoverlapping(src as *const u8, dst.as_mut_ptr(), dst.len());
        }

        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        if src.is_empty() {
            return Ok(());
        }
        let dst = self.range_to_ptr(paddr, src.len())?;

        // Shared-memory (atomics) builds: use atomic byte writes to avoid Rust data-race UB.
        #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
        {
            use core::sync::atomic::{AtomicU8, Ordering};

            let dst = dst as *const AtomicU8;
            for (i, byte) in src.iter().copied().enumerate() {
                // Safety: `range_to_ptr` bounds-checks and `AtomicU8` has alignment 1.
                unsafe { (&*dst.add(i)).store(byte, Ordering::Relaxed) };
            }
        }

        // Non-atomic wasm builds: linear memory is not shared across threads, so memcpy is fine.
        #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len());
        }

        Ok(())
    }

    fn get_slice(&self, _paddr: u64, _len: usize) -> Option<&[u8]> {
        // See the type-level documentation for the safety rationale.
        None
    }

    fn get_slice_mut(&mut self, _paddr: u64, _len: usize) -> Option<&mut [u8]> {
        // See the type-level documentation for the safety rationale.
        None
    }
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

    #[test]
    fn wasm_shared_guest_memory_bounds_and_slices() {
        let mut backing = vec![0u8; 16];
        let mut mem = unsafe { WasmSharedGuestMemory::from_raw_ptr(backing.as_mut_ptr(), 16) }
            .expect("construct WasmSharedGuestMemory");

        // Optional slice fast paths must be disabled for shared wasm memory.
        assert!(mem.get_slice(0, 1).is_none());
        assert!(mem.get_slice_mut(0, 1).is_none());

        // Boundary writes/reads should succeed.
        mem.write_from(12, &[1, 2, 3, 4]).unwrap();
        let mut buf = [0u8; 4];
        mem.read_into(12, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);

        // Reads via helpers must work even without slice fast paths.
        assert_eq!(mem.read_u32_le(12).unwrap(), 0x0403_0201);

        // Out-of-range accesses must return errors (not panic).
        assert!(matches!(
            mem.read_into(15, &mut [0u8; 2]),
            Err(GuestMemoryError::OutOfRange { .. })
        ));
        assert!(matches!(
            mem.write_from(16, &[1u8]),
            Err(GuestMemoryError::OutOfRange { .. })
        ));

        // Overflowing address arithmetic must be handled without panicking.
        assert!(matches!(
            mem.read_into(u64::MAX - 1, &mut [0u8; 2]),
            Err(GuestMemoryError::OutOfRange { .. })
        ));
    }
}
