use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{OnceLock, RwLock};

/// Construction options for [`PhysicalMemory`].
#[derive(Debug, Clone, Copy)]
pub struct PhysicalMemoryOptions {
    /// Allocation granularity.
    ///
    /// The default is 2MiB, which keeps the chunk table small for multi‑GiB RAM
    /// sizes while still allowing sparse allocation.
    pub chunk_size: usize,
}

impl Default for PhysicalMemoryOptions {
    fn default() -> Self {
        Self {
            chunk_size: 2 * 1024 * 1024,
        }
    }
}

/// Errors returned by [`PhysicalMemory`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhysicalMemoryError {
    /// The configured chunk size is invalid.
    InvalidChunkSize { chunk_size: usize },
    /// The requested RAM size cannot be represented on the current platform.
    TooLarge { size: u64, chunk_size: usize },
    /// An access went out of bounds.
    OutOfBounds { addr: u64, len: usize, size: u64 },
}

impl std::fmt::Display for PhysicalMemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhysicalMemoryError::InvalidChunkSize { chunk_size } => {
                write!(
                    f,
                    "invalid chunk size {chunk_size} (must be a power-of-two and >= 4096)"
                )
            }
            PhysicalMemoryError::TooLarge { size, chunk_size } => write!(
                f,
                "physical memory size {size} is too large for this platform with chunk size {chunk_size}"
            ),
            PhysicalMemoryError::OutOfBounds { addr, len, size } => write!(
                f,
                "physical memory access out of bounds: addr={addr} len={len} size={size}"
            ),
        }
    }
}

impl std::error::Error for PhysicalMemoryError {}

struct Chunk {
    bytes: RwLock<Box<[u8]>>,
}

impl Chunk {
    fn new(chunk_size: usize) -> Self {
        Self {
            bytes: RwLock::new(vec![0u8; chunk_size].into_boxed_slice()),
        }
    }
}

/// Sparse guest physical RAM.
///
/// The address space is `[0, len)`. Unallocated chunks read as zeroes and are
/// allocated on first write.
pub struct PhysicalMemory {
    len: u64,
    chunk_size: usize,
    chunk_shift: u32,
    chunk_mask: u64,
    chunks: Vec<OnceLock<Chunk>>,
    allocated_chunks: AtomicUsize,
}

impl std::fmt::Debug for PhysicalMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicalMemory")
            .field("len", &self.len)
            .field("chunk_size", &self.chunk_size)
            .field(
                "allocated_chunks",
                &self.allocated_chunks.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl PhysicalMemory {
    /// Create guest RAM of `size` bytes with default options.
    pub fn new(size: u64) -> Result<Self, PhysicalMemoryError> {
        Self::with_options(size, PhysicalMemoryOptions::default())
    }

    /// Create guest RAM of `size` bytes with explicit options.
    pub fn with_options(
        size: u64,
        options: PhysicalMemoryOptions,
    ) -> Result<Self, PhysicalMemoryError> {
        let chunk_size = options.chunk_size;
        if chunk_size < 4096 || !chunk_size.is_power_of_two() {
            return Err(PhysicalMemoryError::InvalidChunkSize { chunk_size });
        }

        let chunk_shift = chunk_size.trailing_zeros();
        let chunk_mask = (chunk_size as u64) - 1;

        let chunk_count_u64 = size.div_ceil(chunk_size as u64);
        let chunk_count = usize::try_from(chunk_count_u64)
            .map_err(|_| PhysicalMemoryError::TooLarge { size, chunk_size })?;

        let chunks = std::iter::repeat_with(OnceLock::new)
            .take(chunk_count)
            .collect::<Vec<_>>();

        Ok(Self {
            len: size,
            chunk_size,
            chunk_shift,
            chunk_mask,
            chunks,
            allocated_chunks: AtomicUsize::new(0),
        })
    }

    /// Total RAM size (bytes).
    #[inline]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the RAM size is zero.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Chunk size in bytes.
    #[inline]
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Number of chunks currently allocated.
    ///
    /// This is primarily useful for tests and instrumentation.
    #[inline]
    pub fn allocated_chunks(&self) -> usize {
        self.allocated_chunks.load(Ordering::Relaxed)
    }

    #[inline]
    fn check_bounds(&self, addr: u64, len: usize) -> Result<(), PhysicalMemoryError> {
        let end = addr
            .checked_add(len as u64)
            .ok_or(PhysicalMemoryError::OutOfBounds {
                addr,
                len,
                size: self.len,
            })?;
        if end > self.len {
            return Err(PhysicalMemoryError::OutOfBounds {
                addr,
                len,
                size: self.len,
            });
        }
        Ok(())
    }

    #[inline]
    fn chunk_index(&self, addr: u64) -> usize {
        (addr >> self.chunk_shift) as usize
    }

    #[inline]
    fn chunk_offset(&self, addr: u64) -> usize {
        (addr & self.chunk_mask) as usize
    }

    #[inline]
    fn get_chunk(&self, chunk_idx: usize) -> Option<&Chunk> {
        self.chunks.get(chunk_idx)?.get()
    }

    #[inline]
    fn get_or_alloc_chunk(&self, chunk_idx: usize) -> &Chunk {
        self.chunks[chunk_idx].get_or_init(|| {
            self.allocated_chunks.fetch_add(1, Ordering::Relaxed);
            Chunk::new(self.chunk_size)
        })
    }

    /// Read `dst.len()` bytes starting at `addr` into `dst`.
    pub fn try_read_bytes(&self, addr: u64, dst: &mut [u8]) -> Result<(), PhysicalMemoryError> {
        self.check_bounds(addr, dst.len())?;
        if dst.is_empty() {
            return Ok(());
        }

        let mut cur_addr = addr;
        let mut dst_offset = 0usize;

        while dst_offset < dst.len() {
            let chunk_idx = self.chunk_index(cur_addr);
            let chunk_off = self.chunk_offset(cur_addr);

            let available = self.chunk_size - chunk_off;
            let remaining = dst.len() - dst_offset;
            let to_copy = available.min(remaining);

            if let Some(chunk) = self.get_chunk(chunk_idx) {
                let guard = chunk
                    .bytes
                    .read()
                    .expect("physical memory chunk lock poisoned");
                dst[dst_offset..dst_offset + to_copy]
                    .copy_from_slice(&guard[chunk_off..chunk_off + to_copy]);
            } else {
                dst[dst_offset..dst_offset + to_copy].fill(0);
            }

            cur_addr += to_copy as u64;
            dst_offset += to_copy;
        }

        Ok(())
    }

    /// Write `src.len()` bytes starting at `addr` from `src`.
    ///
    /// Chunks are allocated on demand.
    pub fn try_write_bytes(&self, addr: u64, src: &[u8]) -> Result<(), PhysicalMemoryError> {
        self.check_bounds(addr, src.len())?;
        if src.is_empty() {
            return Ok(());
        }

        let mut cur_addr = addr;
        let mut src_offset = 0usize;

        while src_offset < src.len() {
            let chunk_idx = self.chunk_index(cur_addr);
            let chunk_off = self.chunk_offset(cur_addr);

            let available = self.chunk_size - chunk_off;
            let remaining = src.len() - src_offset;
            let to_copy = available.min(remaining);

            let chunk = self.get_or_alloc_chunk(chunk_idx);
            let mut guard = chunk
                .bytes
                .write()
                .expect("physical memory chunk lock poisoned");
            guard[chunk_off..chunk_off + to_copy]
                .copy_from_slice(&src[src_offset..src_offset + to_copy]);

            cur_addr += to_copy as u64;
            src_offset += to_copy;
        }

        Ok(())
    }

    /// Infallible wrapper for [`PhysicalMemory::try_read_bytes`].
    #[inline]
    pub fn read_bytes(&self, addr: u64, dst: &mut [u8]) {
        self.try_read_bytes(addr, dst)
            .expect("physical memory read out of bounds");
    }

    /// Infallible wrapper for [`PhysicalMemory::try_write_bytes`].
    #[inline]
    pub fn write_bytes(&self, addr: u64, src: &[u8]) {
        self.try_write_bytes(addr, src)
            .expect("physical memory write out of bounds");
    }

    pub fn try_read_u8(&self, addr: u64) -> Result<u8, PhysicalMemoryError> {
        let mut buf = [0u8; 1];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(buf[0])
    }

    pub fn try_read_u16(&self, addr: u64) -> Result<u16, PhysicalMemoryError> {
        let mut buf = [0u8; 2];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    pub fn try_read_u32(&self, addr: u64) -> Result<u32, PhysicalMemoryError> {
        let mut buf = [0u8; 4];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    pub fn try_read_u64(&self, addr: u64) -> Result<u64, PhysicalMemoryError> {
        let mut buf = [0u8; 8];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    pub fn try_read_u128(&self, addr: u64) -> Result<u128, PhysicalMemoryError> {
        let mut buf = [0u8; 16];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    pub fn try_write_u8(&self, addr: u64, value: u8) -> Result<(), PhysicalMemoryError> {
        self.try_write_bytes(addr, &[value])
    }

    pub fn try_write_u16(&self, addr: u64, value: u16) -> Result<(), PhysicalMemoryError> {
        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u32(&self, addr: u64, value: u32) -> Result<(), PhysicalMemoryError> {
        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u64(&self, addr: u64, value: u64) -> Result<(), PhysicalMemoryError> {
        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u128(&self, addr: u64, value: u128) -> Result<(), PhysicalMemoryError> {
        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    #[inline]
    pub fn read_u8(&self, addr: u64) -> u8 {
        self.try_read_u8(addr)
            .expect("physical memory read out of bounds")
    }

    #[inline]
    pub fn read_u16(&self, addr: u64) -> u16 {
        self.try_read_u16(addr)
            .expect("physical memory read out of bounds")
    }

    #[inline]
    pub fn read_u32(&self, addr: u64) -> u32 {
        self.try_read_u32(addr)
            .expect("physical memory read out of bounds")
    }

    #[inline]
    pub fn read_u64(&self, addr: u64) -> u64 {
        self.try_read_u64(addr)
            .expect("physical memory read out of bounds")
    }

    #[inline]
    pub fn read_u128(&self, addr: u64) -> u128 {
        self.try_read_u128(addr)
            .expect("physical memory read out of bounds")
    }

    #[inline]
    pub fn write_u8(&self, addr: u64, value: u8) {
        self.try_write_u8(addr, value)
            .expect("physical memory write out of bounds");
    }

    #[inline]
    pub fn write_u16(&self, addr: u64, value: u16) {
        self.try_write_u16(addr, value)
            .expect("physical memory write out of bounds");
    }

    #[inline]
    pub fn write_u32(&self, addr: u64, value: u32) {
        self.try_write_u32(addr, value)
            .expect("physical memory write out of bounds");
    }

    #[inline]
    pub fn write_u64(&self, addr: u64, value: u64) {
        self.try_write_u64(addr, value)
            .expect("physical memory write out of bounds");
    }

    #[inline]
    pub fn write_u128(&self, addr: u64, value: u128) {
        self.try_write_u128(addr, value)
            .expect("physical memory write out of bounds");
    }
}
