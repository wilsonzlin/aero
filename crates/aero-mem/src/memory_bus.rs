use crate::{PhysicalMemory, PhysicalMemoryError};
use std::borrow::Cow;
use std::sync::atomic::{compiler_fence, Ordering};
use std::sync::Arc;

/// MMIO handler registered with [`MemoryBus`].
///
/// The bus guarantees:
/// - `offset` is relative to the registered MMIO base.
/// - The handler will never be asked to access outside its registered range.
/// - Reads/writes are performed with `compiler_fence(Ordering::SeqCst)` around
///   the callback to approximate volatile MMIO semantics.
pub trait MmioHandler: Send + Sync {
    /// Read from MMIO into `data`.
    fn read(&self, offset: u64, data: &mut [u8]);
    /// Write to MMIO from `data`.
    fn write(&self, offset: u64, data: &[u8]);
}

struct FnMmioHandler<R, W> {
    read: R,
    write: W,
}

impl<R, W> MmioHandler for FnMmioHandler<R, W>
where
    R: Fn(u64, &mut [u8]) + Send + Sync,
    W: Fn(u64, &[u8]) + Send + Sync,
{
    fn read(&self, offset: u64, data: &mut [u8]) {
        (self.read)(offset, data);
    }

    fn write(&self, offset: u64, data: &[u8]) {
        (self.write)(offset, data);
    }
}

#[derive(Clone)]
enum OverlayKind {
    Mmio(Arc<dyn MmioHandler>),
    Rom(Arc<[u8]>),
}

#[derive(Clone)]
struct OverlayRegion {
    start: u64,
    end: u64,
    kind: OverlayKind,
}

impl OverlayRegion {
    #[inline]
    fn contains(&self, addr: u64) -> bool {
        addr >= self.start && addr < self.end
    }
}

/// Errors returned by [`MemoryBus`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryBusError {
    /// The given MMIO/ROM range is invalid.
    InvalidRange { start: u64, end: u64 },
    /// The new mapping overlaps with an existing MMIO/ROM region.
    Overlap {
        start: u64,
        end: u64,
        existing_start: u64,
        existing_end: u64,
    },
    /// `addr + len` overflowed `u64`.
    AddressOverflow { addr: u64, len: usize },
    /// The access is not backed by RAM, MMIO, or ROM.
    Unmapped { addr: u64, len: usize },
    /// The access fell within RAM but exceeded the guest RAM size.
    RamOutOfBounds(PhysicalMemoryError),
    /// A DMA-style RAM-only operation overlaps an MMIO region.
    MmioAccess { addr: u64, len: usize },
    /// A DMA-style RAM-only operation overlaps a ROM region.
    RomAccess { addr: u64, len: usize },
    /// Scatter/gather total length overflowed `usize`.
    LengthOverflow,
    /// Scatter/gather buffer length mismatch.
    LengthMismatch { expected: usize, actual: usize },
    /// Failed to allocate a host buffer for the requested operation.
    OutOfMemory { len: usize },
}

impl std::fmt::Display for MemoryBusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryBusError::InvalidRange { start, end } => {
                write!(f, "invalid memory region range {start:#x}..{end:#x}")
            }
            MemoryBusError::Overlap {
                start,
                end,
                existing_start,
                existing_end,
            } => write!(
                f,
                "memory region {start:#x}..{end:#x} overlaps with existing {existing_start:#x}..{existing_end:#x}"
            ),
            MemoryBusError::AddressOverflow { addr, len } => {
                write!(f, "address overflow: addr={addr:#x} len={len}")
            }
            MemoryBusError::Unmapped { addr, len } => {
                write!(f, "unmapped memory access: addr={addr:#x} len={len}")
            }
            MemoryBusError::RamOutOfBounds(err) => write!(f, "{err}"),
            MemoryBusError::MmioAccess { addr, len } => {
                write!(f, "DMA access overlaps MMIO: addr={addr:#x} len={len}")
            }
            MemoryBusError::RomAccess { addr, len } => {
                write!(f, "DMA access overlaps ROM: addr={addr:#x} len={len}")
            }
            MemoryBusError::LengthOverflow => write!(f, "scatter/gather length overflow"),
            MemoryBusError::LengthMismatch { expected, actual } => write!(
                f,
                "scatter/gather length mismatch: expected={expected} actual={actual}"
            ),
            MemoryBusError::OutOfMemory { len } => write!(f, "out of memory allocating {len} bytes"),
        }
    }
}

impl std::error::Error for MemoryBusError {}

impl From<PhysicalMemoryError> for MemoryBusError {
    fn from(value: PhysicalMemoryError) -> Self {
        Self::RamOutOfBounds(value)
    }
}

/// Physical address routing layer.
///
/// The bus always checks MMIO regions first, then ROM, then RAM.
#[derive(Clone)]
pub struct MemoryBus {
    ram: Arc<PhysicalMemory>,
    overlays: Vec<OverlayRegion>, // sorted by start, disjoint
}

impl std::fmt::Debug for MemoryBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryBus")
            .field("ram_len", &self.ram.len())
            .field("overlay_count", &self.overlays.len())
            .finish_non_exhaustive()
    }
}

impl MemoryBus {
    /// Create a bus that exposes the provided RAM and has no ROM/MMIO mappings.
    pub fn new(ram: Arc<PhysicalMemory>) -> Self {
        Self {
            ram,
            overlays: Vec::new(),
        }
    }

    /// Register a MMIO region.
    pub fn register_mmio(
        &mut self,
        range: std::ops::Range<u64>,
        handler: Arc<dyn MmioHandler>,
    ) -> Result<(), MemoryBusError> {
        self.insert_overlay(OverlayRegion {
            start: range.start,
            end: range.end,
            kind: OverlayKind::Mmio(handler),
        })
    }

    /// Register a MMIO region backed by closures.
    pub fn register_mmio_fn<R, W>(
        &mut self,
        range: std::ops::Range<u64>,
        read: R,
        write: W,
    ) -> Result<(), MemoryBusError>
    where
        R: Fn(u64, &mut [u8]) + Send + Sync + 'static,
        W: Fn(u64, &[u8]) + Send + Sync + 'static,
    {
        let handler: Arc<dyn MmioHandler> = Arc::new(FnMmioHandler { read, write });
        self.register_mmio(range, handler)
    }

    /// Register an "open bus" region: reads return `0xFF`, writes are ignored.
    ///
    /// This is useful for reserving memory holes (e.g. PCI MMIO space) without
    /// attaching a device model yet.
    pub fn register_open_bus(&mut self, range: std::ops::Range<u64>) -> Result<(), MemoryBusError> {
        self.register_mmio_fn(range, |_offset, data| data.fill(0xFF), |_offset, _data| {})
    }

    /// Register a read-only ROM region.
    pub fn register_rom(&mut self, start: u64, data: Arc<[u8]>) -> Result<(), MemoryBusError> {
        let len_u64 = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let end = start
            .checked_add(len_u64)
            .ok_or(MemoryBusError::InvalidRange { start, end: start })?;
        if start >= end {
            return Err(MemoryBusError::InvalidRange { start, end });
        }

        self.insert_overlay(OverlayRegion {
            start,
            end,
            kind: OverlayKind::Rom(data),
        })
    }

    #[inline]
    pub fn ram(&self) -> &PhysicalMemory {
        &self.ram
    }

    #[inline]
    fn checked_end(addr: u64, len: usize) -> Result<u64, MemoryBusError> {
        addr.checked_add(len as u64)
            .ok_or(MemoryBusError::AddressOverflow { addr, len })
    }

    fn overlapping_overlay(&self, start: u64, end: u64) -> Option<&OverlayRegion> {
        debug_assert!(start < end);

        // Find the first region whose start is >= end; any overlap must be in idx-1 because
        // regions are disjoint and sorted by `start`.
        let idx = self.overlays.partition_point(|r| r.start < end);
        if idx == 0 {
            return None;
        }

        let candidate = &self.overlays[idx - 1];
        if candidate.end > start {
            Some(candidate)
        } else {
            None
        }
    }

    fn check_dma_ram_range(&self, addr: u64, len: usize) -> Result<(), MemoryBusError> {
        if len == 0 {
            return Ok(());
        }

        let end = Self::checked_end(addr, len)?;
        if end > self.ram.len() {
            return Err(MemoryBusError::RamOutOfBounds(
                PhysicalMemoryError::OutOfBounds {
                    addr,
                    len,
                    size: self.ram.len(),
                },
            ));
        }

        if let Some(region) = self.overlapping_overlay(addr, end) {
            return Err(match &region.kind {
                OverlayKind::Mmio(_) => MemoryBusError::MmioAccess { addr, len },
                OverlayKind::Rom(_) => MemoryBusError::RomAccess { addr, len },
            });
        }

        Ok(())
    }

    fn insert_overlay(&mut self, region: OverlayRegion) -> Result<(), MemoryBusError> {
        if region.start >= region.end {
            return Err(MemoryBusError::InvalidRange {
                start: region.start,
                end: region.end,
            });
        }

        // Insertion point by start address.
        let idx = self.overlays.partition_point(|r| r.start < region.start);

        if idx > 0 {
            let prev = &self.overlays[idx - 1];
            if prev.end > region.start {
                return Err(MemoryBusError::Overlap {
                    start: region.start,
                    end: region.end,
                    existing_start: prev.start,
                    existing_end: prev.end,
                });
            }
        }
        if idx < self.overlays.len() {
            let next = &self.overlays[idx];
            if region.end > next.start {
                return Err(MemoryBusError::Overlap {
                    start: region.start,
                    end: region.end,
                    existing_start: next.start,
                    existing_end: next.end,
                });
            }
        }

        self.overlays.insert(idx, region);
        Ok(())
    }

    #[inline]
    fn find_overlay_idx(&self, addr: u64) -> Option<usize> {
        let idx = self.overlays.partition_point(|r| r.start <= addr);
        if idx == 0 {
            return None;
        }
        let candidate = &self.overlays[idx - 1];
        if candidate.contains(addr) {
            Some(idx - 1)
        } else {
            None
        }
    }

    #[inline]
    fn next_overlay_start_after(&self, addr: u64) -> Option<u64> {
        let idx = self.overlays.partition_point(|r| r.start <= addr);
        self.overlays.get(idx).map(|r| r.start)
    }

    /// Bulk read helper.
    pub fn try_read_bytes(&self, addr: u64, dst: &mut [u8]) -> Result<(), MemoryBusError> {
        if dst.is_empty() {
            return Ok(());
        }

        let end = Self::checked_end(addr, dst.len())?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            self.ram.try_read_bytes(addr, dst)?;
            return Ok(());
        }

        let mut cur_addr = addr;
        let mut dst_offset = 0usize;

        while dst_offset < dst.len() {
            if let Some(overlay_idx) = self.find_overlay_idx(cur_addr) {
                let overlay = &self.overlays[overlay_idx];
                let max_len = overlay.end - cur_addr;
                let remaining = (dst.len() - dst_offset) as u64;
                let seg_len = std::cmp::min(max_len, remaining) as usize;
                let offset = cur_addr - overlay.start;

                match &overlay.kind {
                    OverlayKind::Rom(data) => {
                        let base = usize::try_from(offset).expect("ROM offset overflow");
                        dst[dst_offset..dst_offset + seg_len]
                            .copy_from_slice(&data[base..base + seg_len]);
                    }
                    OverlayKind::Mmio(handler) => {
                        compiler_fence(Ordering::SeqCst);
                        handler.read(offset, &mut dst[dst_offset..dst_offset + seg_len]);
                        compiler_fence(Ordering::SeqCst);
                    }
                }

                cur_addr += seg_len as u64;
                dst_offset += seg_len;
                continue;
            }

            // RAM path.
            if cur_addr >= self.ram.len() {
                return Err(MemoryBusError::Unmapped {
                    addr: cur_addr,
                    len: dst.len() - dst_offset,
                });
            }

            let ram_end = self.ram.len();
            let next_overlay = self.next_overlay_start_after(cur_addr).unwrap_or(ram_end);
            let seg_end = std::cmp::min(ram_end, next_overlay);
            let max_len = seg_end - cur_addr;
            let remaining = (dst.len() - dst_offset) as u64;
            let seg_len = std::cmp::min(max_len, remaining) as usize;

            self.ram
                .try_read_bytes(cur_addr, &mut dst[dst_offset..dst_offset + seg_len])?;

            cur_addr += seg_len as u64;
            dst_offset += seg_len;
        }

        Ok(())
    }

    /// Bulk write helper.
    pub fn try_write_bytes(&self, addr: u64, src: &[u8]) -> Result<(), MemoryBusError> {
        if src.is_empty() {
            return Ok(());
        }

        let end = Self::checked_end(addr, src.len())?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            self.ram.try_write_bytes(addr, src)?;
            return Ok(());
        }

        let mut cur_addr = addr;
        let mut src_offset = 0usize;

        while src_offset < src.len() {
            if let Some(overlay_idx) = self.find_overlay_idx(cur_addr) {
                let overlay = &self.overlays[overlay_idx];
                let max_len = overlay.end - cur_addr;
                let remaining = (src.len() - src_offset) as u64;
                let seg_len = std::cmp::min(max_len, remaining) as usize;
                let offset = cur_addr - overlay.start;

                match &overlay.kind {
                    OverlayKind::Rom(_) => {
                        // Writes to ROM are ignored.
                    }
                    OverlayKind::Mmio(handler) => {
                        compiler_fence(Ordering::SeqCst);
                        handler.write(offset, &src[src_offset..src_offset + seg_len]);
                        compiler_fence(Ordering::SeqCst);
                    }
                }

                cur_addr += seg_len as u64;
                src_offset += seg_len;
                continue;
            }

            // RAM path.
            if cur_addr >= self.ram.len() {
                return Err(MemoryBusError::Unmapped {
                    addr: cur_addr,
                    len: src.len() - src_offset,
                });
            }

            let ram_end = self.ram.len();
            let next_overlay = self.next_overlay_start_after(cur_addr).unwrap_or(ram_end);
            let seg_end = std::cmp::min(ram_end, next_overlay);
            let max_len = seg_end - cur_addr;
            let remaining = (src.len() - src_offset) as u64;
            let seg_len = std::cmp::min(max_len, remaining) as usize;

            self.ram
                .try_write_bytes(cur_addr, &src[src_offset..src_offset + seg_len])?;

            cur_addr += seg_len as u64;
            src_offset += seg_len;
        }

        Ok(())
    }

    /// Bulk read restricted to guest RAM.
    ///
    /// This is intended for DMA-style paths (disk/network) that must not trigger
    /// MMIO side effects and must not read from ROM.
    pub fn try_read_ram_bytes(&self, addr: u64, dst: &mut [u8]) -> Result<(), MemoryBusError> {
        self.check_dma_ram_range(addr, dst.len())?;
        self.ram.try_read_bytes(addr, dst)?;
        Ok(())
    }

    /// Bulk write restricted to guest RAM.
    ///
    /// This is intended for DMA-style paths (disk/network) that must not trigger
    /// MMIO side effects and must not write into ROM/MMIO.
    pub fn try_write_ram_bytes(&self, addr: u64, src: &[u8]) -> Result<(), MemoryBusError> {
        self.check_dma_ram_range(addr, src.len())?;
        self.ram.try_write_bytes(addr, src)?;
        Ok(())
    }

    fn validate_sg(&self, segments: &[(u64, usize)], buf_len: usize) -> Result<(), MemoryBusError> {
        let mut total = 0usize;
        for (_, len) in segments {
            total = total
                .checked_add(*len)
                .ok_or(MemoryBusError::LengthOverflow)?;
        }
        if total != buf_len {
            return Err(MemoryBusError::LengthMismatch {
                expected: total,
                actual: buf_len,
            });
        }
        for (addr, len) in segments {
            self.check_dma_ram_range(*addr, *len)?;
        }
        Ok(())
    }

    /// Scatter/gather read restricted to guest RAM.
    pub fn try_read_sg(
        &self,
        segments: &[(u64, usize)],
        dst: &mut [u8],
    ) -> Result<(), MemoryBusError> {
        self.validate_sg(segments, dst.len())?;

        let mut out = dst;
        for (addr, len) in segments {
            let (head, tail) = out.split_at_mut(*len);
            self.ram.try_read_bytes(*addr, head)?;
            out = tail;
        }
        Ok(())
    }

    /// Scatter/gather write restricted to guest RAM.
    pub fn try_write_sg(
        &self,
        segments: &[(u64, usize)],
        src: &[u8],
    ) -> Result<(), MemoryBusError> {
        self.validate_sg(segments, src.len())?;

        let mut input = src;
        for (addr, len) in segments {
            let (head, tail) = input.split_at(*len);
            self.ram.try_write_bytes(*addr, head)?;
            input = tail;
        }
        Ok(())
    }

    #[inline]
    pub fn read_bytes(&self, addr: u64, dst: &mut [u8]) {
        self.try_read_bytes(addr, dst)
            .expect("memory bus read failed");
    }

    #[inline]
    pub fn write_bytes(&self, addr: u64, src: &[u8]) {
        self.try_write_bytes(addr, src)
            .expect("memory bus write failed");
    }

    /// DMA-friendly bulk RAM read.
    ///
    /// This is provided for callers that expect the "read_physical_into" naming
    /// convention from other emulators.
    #[inline]
    pub fn read_physical_into(&self, addr: u64, dst: &mut [u8]) -> Result<(), MemoryBusError> {
        self.try_read_ram_bytes(addr, dst)
    }

    /// DMA-friendly bulk RAM write.
    ///
    /// This is provided for callers that expect the "write_physical_from" naming
    /// convention from other emulators.
    #[inline]
    pub fn write_physical_from(&self, addr: u64, src: &[u8]) -> Result<(), MemoryBusError> {
        self.try_write_ram_bytes(addr, src)
    }

    /// Copy guest bytes into an owned/borrowed buffer.
    ///
    /// Currently this always returns an owned buffer because the underlying
    /// [`PhysicalMemory`] is chunked behind locks, so handing out direct borrows
    /// would require lifetime-carrying guards.
    pub fn memcpy_from_guest<'a>(
        &'a self,
        addr: u64,
        len: usize,
    ) -> Result<Cow<'a, [u8]>, MemoryBusError> {
        if len == 0 {
            return Ok(Cow::Borrowed(&[]));
        }
        let mut buf = Vec::new();
        buf.try_reserve_exact(len)
            .map_err(|_| MemoryBusError::OutOfMemory { len })?;
        buf.resize(len, 0);
        self.try_read_ram_bytes(addr, &mut buf)?;
        Ok(Cow::Owned(buf))
    }

    /// Scatter/gather read helper.
    ///
    /// `dst.len()` must equal the sum of all segment lengths.
    pub fn read_sg(&self, segments: &[(u64, usize)], dst: &mut [u8]) -> Result<(), MemoryBusError> {
        self.try_read_sg(segments, dst)
    }

    /// Scatter/gather write helper.
    ///
    /// `src.len()` must equal the sum of all segment lengths.
    pub fn write_sg(&self, segments: &[(u64, usize)], src: &[u8]) -> Result<(), MemoryBusError> {
        self.try_write_sg(segments, src)
    }

    pub fn try_read_u8(&self, addr: u64) -> Result<u8, MemoryBusError> {
        let end = Self::checked_end(addr, 1)?;
        if end <= self.ram.len() && self.find_overlay_idx(addr).is_none() {
            return Ok(self.ram.try_read_u8(addr)?);
        }

        let mut buf = [0u8; 1];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(buf[0])
    }

    pub fn try_read_u16(&self, addr: u64) -> Result<u16, MemoryBusError> {
        let end = Self::checked_end(addr, 2)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_read_u16(addr)?);
        }

        let mut buf = [0u8; 2];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    pub fn try_read_u32(&self, addr: u64) -> Result<u32, MemoryBusError> {
        let end = Self::checked_end(addr, 4)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_read_u32(addr)?);
        }

        let mut buf = [0u8; 4];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    pub fn try_read_u64(&self, addr: u64) -> Result<u64, MemoryBusError> {
        let end = Self::checked_end(addr, 8)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_read_u64(addr)?);
        }

        let mut buf = [0u8; 8];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    pub fn try_read_u128(&self, addr: u64) -> Result<u128, MemoryBusError> {
        let end = Self::checked_end(addr, 16)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_read_u128(addr)?);
        }

        let mut buf = [0u8; 16];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    pub fn try_write_u8(&self, addr: u64, value: u8) -> Result<(), MemoryBusError> {
        let end = Self::checked_end(addr, 1)?;
        if end <= self.ram.len() && self.find_overlay_idx(addr).is_none() {
            return Ok(self.ram.try_write_u8(addr, value)?);
        }

        self.try_write_bytes(addr, &[value])
    }

    pub fn try_write_u16(&self, addr: u64, value: u16) -> Result<(), MemoryBusError> {
        let end = Self::checked_end(addr, 2)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_write_u16(addr, value)?);
        }

        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u32(&self, addr: u64, value: u32) -> Result<(), MemoryBusError> {
        let end = Self::checked_end(addr, 4)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_write_u32(addr, value)?);
        }

        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u64(&self, addr: u64, value: u64) -> Result<(), MemoryBusError> {
        let end = Self::checked_end(addr, 8)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_write_u64(addr, value)?);
        }

        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u128(&self, addr: u64, value: u128) -> Result<(), MemoryBusError> {
        let end = Self::checked_end(addr, 16)?;
        if end <= self.ram.len() && self.overlapping_overlay(addr, end).is_none() {
            return Ok(self.ram.try_write_u128(addr, value)?);
        }

        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    #[inline]
    pub fn read_u8(&self, addr: u64) -> u8 {
        self.try_read_u8(addr).expect("memory bus read failed")
    }

    #[inline]
    pub fn read_u16(&self, addr: u64) -> u16 {
        self.try_read_u16(addr).expect("memory bus read failed")
    }

    #[inline]
    pub fn read_u32(&self, addr: u64) -> u32 {
        self.try_read_u32(addr).expect("memory bus read failed")
    }

    #[inline]
    pub fn read_u64(&self, addr: u64) -> u64 {
        self.try_read_u64(addr).expect("memory bus read failed")
    }

    #[inline]
    pub fn read_u128(&self, addr: u64) -> u128 {
        self.try_read_u128(addr).expect("memory bus read failed")
    }

    #[inline]
    pub fn write_u8(&self, addr: u64, value: u8) {
        self.try_write_u8(addr, value)
            .expect("memory bus write failed");
    }

    #[inline]
    pub fn write_u16(&self, addr: u64, value: u16) {
        self.try_write_u16(addr, value)
            .expect("memory bus write failed");
    }

    #[inline]
    pub fn write_u32(&self, addr: u64, value: u32) {
        self.try_write_u32(addr, value)
            .expect("memory bus write failed");
    }

    #[inline]
    pub fn write_u64(&self, addr: u64, value: u64) {
        self.try_write_u64(addr, value)
            .expect("memory bus write failed");
    }

    #[inline]
    pub fn write_u128(&self, addr: u64, value: u128) {
        self.try_write_u128(addr, value)
            .expect("memory bus write failed");
    }
}
