use std::borrow::Cow;
use std::error::Error;
use std::fmt;

use memory::{GuestMemory, GuestMemoryError};

pub type MemoryBusResult<T> = Result<T, MemoryBusError>;

/// Errors returned by [`MemoryBus`] DMA helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryBusError {
    Ram(GuestMemoryError),
    AddressOverflow { paddr: u64, len: usize },
    RomAccess { paddr: u64, len: usize },
    MmioAccess { paddr: u64, len: usize },
    LengthOverflow,
    LengthMismatch { expected: usize, actual: usize },
}

impl fmt::Display for MemoryBusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryBusError::Ram(err) => write!(f, "{err}"),
            MemoryBusError::AddressOverflow { paddr, len } => {
                write!(f, "address overflow: paddr=0x{paddr:x} len={len}")
            }
            MemoryBusError::RomAccess { paddr, len } => {
                write!(f, "DMA access overlaps ROM: paddr=0x{paddr:x} len={len}")
            }
            MemoryBusError::MmioAccess { paddr, len } => {
                write!(f, "DMA access overlaps MMIO: paddr=0x{paddr:x} len={len}")
            }
            MemoryBusError::LengthOverflow => write!(f, "scatter/gather length overflow"),
            MemoryBusError::LengthMismatch { expected, actual } => write!(
                f,
                "scatter/gather length mismatch: expected={expected} actual={actual}"
            ),
        }
    }
}

impl Error for MemoryBusError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            MemoryBusError::Ram(err) => Some(err),
            _ => None,
        }
    }
}

impl From<GuestMemoryError> for MemoryBusError {
    fn from(value: GuestMemoryError) -> Self {
        MemoryBusError::Ram(value)
    }
}

/// A memory-mapped I/O handler.
///
/// These calls may have side-effects, so MMIO reads require `&mut self`.
pub trait MmioHandler {
    fn read_u8(&mut self, offset: u64) -> u8;
    fn write_u8(&mut self, offset: u64, value: u8);
}

struct MmioRegion {
    start: u64,
    len: u64,
    handler: Box<dyn MmioHandler>,
}

impl MmioRegion {
    fn contains(&self, paddr: u64) -> bool {
        paddr >= self.start && paddr < self.start + self.len
    }
}

#[derive(Clone)]
struct RomRegion {
    start: u64,
    data: Vec<u8>,
}

impl RomRegion {
    fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    fn contains(&self, paddr: u64) -> bool {
        paddr >= self.start && paddr < self.end()
    }
}

/// System physical memory bus (RAM + ROM + MMIO).
///
/// DMA helpers (`read_physical_into`, `write_physical_from`, `read_sg`, `write_sg`) are restricted
/// to guest RAM; attempting to touch ROM/MMIO returns an error instead of invoking side-effects.
pub struct MemoryBus {
    ram: Box<dyn GuestMemory>,
    rom: Vec<RomRegion>,
    mmio: Vec<MmioRegion>,
}

impl MemoryBus {
    pub fn new(ram: Box<dyn GuestMemory>) -> Self {
        Self {
            ram,
            rom: Vec::new(),
            mmio: Vec::new(),
        }
    }

    pub fn ram(&self) -> &dyn GuestMemory {
        &*self.ram
    }

    pub fn ram_mut(&mut self) -> &mut dyn GuestMemory {
        &mut *self.ram
    }

    pub fn add_rom_region(&mut self, start: u64, data: Vec<u8>) {
        self.rom.push(RomRegion { start, data });
    }

    pub fn add_mmio_region(&mut self, start: u64, len: u64, handler: Box<dyn MmioHandler>) {
        self.mmio.push(MmioRegion {
            start,
            len,
            handler,
        });
    }

    fn check_dma_ram_range(&self, paddr: u64, len: usize) -> MemoryBusResult<()> {
        if len == 0 {
            return Ok(());
        }

        let len_u64 = len as u64;
        let end = paddr
            .checked_add(len_u64)
            .ok_or(MemoryBusError::AddressOverflow { paddr, len })?;

        for region in &self.mmio {
            if ranges_overlap(paddr, end, region.start, region.start + region.len) {
                return Err(MemoryBusError::MmioAccess { paddr, len });
            }
        }
        for region in &self.rom {
            if ranges_overlap(paddr, end, region.start, region.end()) {
                return Err(MemoryBusError::RomAccess { paddr, len });
            }
        }

        let size = self.ram.size();
        if end > size {
            return Err(MemoryBusError::Ram(GuestMemoryError::OutOfRange {
                paddr,
                len,
                size,
            }));
        }

        Ok(())
    }

    /// Reads guest RAM at `paddr` into `dst`.
    pub fn read_physical_into(&self, paddr: u64, dst: &mut [u8]) -> MemoryBusResult<()> {
        self.check_dma_ram_range(paddr, dst.len())?;
        if let Some(src) = self.ram.get_slice(paddr, dst.len()) {
            dst.copy_from_slice(src);
            return Ok(());
        }
        self.ram.read_into(paddr, dst)?;
        Ok(())
    }

    /// Writes `src` into guest RAM at `paddr`.
    pub fn write_physical_from(&mut self, paddr: u64, src: &[u8]) -> MemoryBusResult<()> {
        self.check_dma_ram_range(paddr, src.len())?;
        if let Some(dst) = self.ram.get_slice_mut(paddr, src.len()) {
            dst.copy_from_slice(src);
            return Ok(());
        }
        self.ram.write_from(paddr, src)?;
        Ok(())
    }

    pub fn memcpy_from_guest<'a>(
        &'a self,
        paddr: u64,
        len: usize,
    ) -> MemoryBusResult<Cow<'a, [u8]>> {
        self.check_dma_ram_range(paddr, len)?;
        if let Some(src) = self.ram.get_slice(paddr, len) {
            return Ok(Cow::Borrowed(src));
        }
        let mut buf = vec![0u8; len];
        self.ram.read_into(paddr, &mut buf)?;
        Ok(Cow::Owned(buf))
    }

    pub fn read_sg(&self, segments: &[(u64, usize)], dst: &mut [u8]) -> MemoryBusResult<()> {
        self.validate_sg(segments, dst.len())?;

        let mut out = dst;
        for (paddr, len) in segments {
            let (head, tail) = out.split_at_mut(*len);
            self.ram.read_into(*paddr, head)?;
            out = tail;
        }
        Ok(())
    }

    pub fn write_sg(&mut self, segments: &[(u64, usize)], src: &[u8]) -> MemoryBusResult<()> {
        self.validate_sg(segments, src.len())?;

        let mut input = src;
        for (paddr, len) in segments {
            let (head, tail) = input.split_at(*len);
            self.ram.write_from(*paddr, head)?;
            input = tail;
        }
        Ok(())
    }

    fn validate_sg(&self, segments: &[(u64, usize)], buf_len: usize) -> MemoryBusResult<()> {
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
        for (paddr, len) in segments {
            self.check_dma_ram_range(*paddr, *len)?;
        }
        Ok(())
    }

    pub fn read_physical_u8(&mut self, paddr: u64) -> MemoryBusResult<u8> {
        if let Some(idx) = self.mmio.iter().position(|r| r.contains(paddr)) {
            let region_start = self.mmio[idx].start;
            return Ok(self.mmio[idx].handler.read_u8(paddr - region_start));
        }
        if let Some(region) = self.rom.iter().find(|r| r.contains(paddr)) {
            let offset = (paddr - region.start) as usize;
            return Ok(region.data[offset]);
        }
        Ok(self.ram.read_u8_le(paddr)?)
    }

    pub fn write_physical_u8(&mut self, paddr: u64, value: u8) -> MemoryBusResult<()> {
        if let Some(idx) = self.mmio.iter().position(|r| r.contains(paddr)) {
            let region_start = self.mmio[idx].start;
            self.mmio[idx].handler.write_u8(paddr - region_start, value);
            return Ok(());
        }
        if self.rom.iter().any(|r| r.contains(paddr)) {
            return Ok(());
        }
        self.ram.write_u8_le(paddr, value)?;
        Ok(())
    }
}

fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start < b_end && b_start < a_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory::{DenseMemory, SparseMemory};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn bulk_read_write_within_ram() {
        let ram = DenseMemory::new(8 * 1024 * 1024).unwrap();
        let mut bus = MemoryBus::new(Box::new(ram));

        let src: Vec<u8> = (0..1024).map(|i| i as u8).collect();
        bus.write_physical_from(0x1000, &src).unwrap();

        let mut dst = vec![0u8; src.len()];
        bus.read_physical_into(0x1000, &mut dst).unwrap();
        assert_eq!(dst, src);
    }

    #[test]
    fn bulk_crosses_sparse_chunk_boundary() {
        let ram = SparseMemory::with_chunk_size(64, 16).unwrap();
        let mut bus = MemoryBus::new(Box::new(ram));

        let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
        bus.write_physical_from(12, &src).unwrap();

        let mut dst = [0u8; 8];
        bus.read_physical_into(12, &mut dst).unwrap();
        assert_eq!(dst, src);
    }

    #[test]
    fn scatter_gather_roundtrip() {
        let ram = DenseMemory::new(0x4000).unwrap();
        let mut bus = MemoryBus::new(Box::new(ram));

        let segments = &[(0x1000, 16), (0x1100, 8), (0x1200, 32)];
        let total: usize = segments.iter().map(|(_, len)| *len).sum();
        let src: Vec<u8> = (0..total).map(|i| (i ^ 0x5a) as u8).collect();

        bus.write_sg(segments, &src).unwrap();

        let mut dst = vec![0u8; total];
        bus.read_sg(segments, &mut dst).unwrap();
        assert_eq!(dst, src);
    }

    struct CountingMmio {
        reads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
    }

    impl MmioHandler for CountingMmio {
        fn read_u8(&mut self, _offset: u64) -> u8 {
            self.reads.fetch_add(1, Ordering::Relaxed);
            0xcc
        }

        fn write_u8(&mut self, _offset: u64, _value: u8) {
            self.writes.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn sg_rejects_mmio_without_side_effects_or_partial_write() {
        let reads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let ram = DenseMemory::new(0x4000).unwrap();
        let mut bus = MemoryBus::new(Box::new(ram));

        bus.add_mmio_region(
            0x2000,
            0x100,
            Box::new(CountingMmio {
                reads: reads.clone(),
                writes: writes.clone(),
            }),
        );

        let segments = &[(0x1000, 4), (0x2000, 4)];
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8];

        let err = bus.write_sg(segments, &src).unwrap_err();
        assert!(matches!(err, MemoryBusError::MmioAccess { .. }));

        let mut dst = [0u8; 4];
        bus.read_physical_into(0x1000, &mut dst).unwrap();
        assert_eq!(dst, [0u8; 4]);

        assert_eq!(reads.load(Ordering::Relaxed), 0);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn sg_rejects_rom_without_partial_read() {
        let ram = DenseMemory::new(0x4000).unwrap();
        let mut bus = MemoryBus::new(Box::new(ram));
        bus.add_rom_region(0x3000, vec![0x11, 0x22, 0x33, 0x44]);

        let segments = &[(0x1000, 4), (0x3000, 4)];
        let mut dst = [0xaau8; 8];
        let err = bus.read_sg(segments, &mut dst).unwrap_err();
        assert!(matches!(err, MemoryBusError::RomAccess { .. }));
        assert_eq!(dst, [0xaau8; 8]);
    }
}
