use crate::{PhysicalMemory, PhysicalMemoryError};
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
#[derive(Debug)]
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
    /// The access is not backed by RAM, MMIO, or ROM.
    Unmapped { addr: u64, len: usize },
    /// The access fell within RAM but exceeded the guest RAM size.
    RamOutOfBounds(PhysicalMemoryError),
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
            MemoryBusError::Unmapped { addr, len } => {
                write!(f, "unmapped memory access: addr={addr:#x} len={len}")
            }
            MemoryBusError::RamOutOfBounds(err) => write!(f, "{err}"),
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
                        // Most MMIO accesses are small; for larger ranges fall back to per-byte.
                        if seg_len <= 16 {
                            compiler_fence(Ordering::SeqCst);
                            handler.read(offset, &mut dst[dst_offset..dst_offset + seg_len]);
                            compiler_fence(Ordering::SeqCst);
                        } else {
                            for i in 0..seg_len {
                                compiler_fence(Ordering::SeqCst);
                                handler.read(
                                    offset + i as u64,
                                    &mut dst[dst_offset + i..dst_offset + i + 1],
                                );
                                compiler_fence(Ordering::SeqCst);
                            }
                        }
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
                        if seg_len <= 16 {
                            compiler_fence(Ordering::SeqCst);
                            handler.write(offset, &src[src_offset..src_offset + seg_len]);
                            compiler_fence(Ordering::SeqCst);
                        } else {
                            for i in 0..seg_len {
                                compiler_fence(Ordering::SeqCst);
                                handler.write(
                                    offset + i as u64,
                                    &src[src_offset + i..src_offset + i + 1],
                                );
                                compiler_fence(Ordering::SeqCst);
                            }
                        }
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

    pub fn try_read_u8(&self, addr: u64) -> Result<u8, MemoryBusError> {
        let mut buf = [0u8; 1];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(buf[0])
    }

    pub fn try_read_u16(&self, addr: u64) -> Result<u16, MemoryBusError> {
        let mut buf = [0u8; 2];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    pub fn try_read_u32(&self, addr: u64) -> Result<u32, MemoryBusError> {
        let mut buf = [0u8; 4];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    pub fn try_read_u64(&self, addr: u64) -> Result<u64, MemoryBusError> {
        let mut buf = [0u8; 8];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    pub fn try_read_u128(&self, addr: u64) -> Result<u128, MemoryBusError> {
        let mut buf = [0u8; 16];
        self.try_read_bytes(addr, &mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    pub fn try_write_u8(&self, addr: u64, value: u8) -> Result<(), MemoryBusError> {
        self.try_write_bytes(addr, &[value])
    }

    pub fn try_write_u16(&self, addr: u64, value: u16) -> Result<(), MemoryBusError> {
        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u32(&self, addr: u64, value: u32) -> Result<(), MemoryBusError> {
        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u64(&self, addr: u64, value: u64) -> Result<(), MemoryBusError> {
        self.try_write_bytes(addr, &value.to_le_bytes())
    }

    pub fn try_write_u128(&self, addr: u64, value: u128) -> Result<(), MemoryBusError> {
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
