use crate::phys::GuestMemory;
use std::sync::Arc;

/// Abstraction for guest physical memory access.
///
/// The MMU performs page table walks by reading and writing guest physical
/// memory. Real systems may map page tables to RAM or MMIO; therefore reads are
/// defined as `&mut self` to allow implementations with side effects.
pub trait MemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]);
    fn write_physical(&mut self, paddr: u64, buf: &[u8]);

    fn read_u8(&mut self, paddr: u64) -> u8 {
        let mut buf = [0u8; 1];
        self.read_physical(paddr, &mut buf);
        buf[0]
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        let mut buf = [0u8; 2];
        self.read_physical(paddr, &mut buf);
        u16::from_le_bytes(buf)
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        let mut buf = [0u8; 4];
        self.read_physical(paddr, &mut buf);
        u32::from_le_bytes(buf)
    }

    fn read_u64(&mut self, paddr: u64) -> u64 {
        let mut buf = [0u8; 8];
        self.read_physical(paddr, &mut buf);
        u64::from_le_bytes(buf)
    }

    fn read_u128(&mut self, paddr: u64) -> u128 {
        let mut buf = [0u8; 16];
        self.read_physical(paddr, &mut buf);
        u128::from_le_bytes(buf)
    }

    fn write_u8(&mut self, paddr: u64, val: u8) {
        self.write_physical(paddr, &[val]);
    }

    fn write_u16(&mut self, paddr: u64, val: u16) {
        self.write_physical(paddr, &val.to_le_bytes());
    }

    fn write_u32(&mut self, paddr: u64, val: u32) {
        self.write_physical(paddr, &val.to_le_bytes());
    }

    fn write_u64(&mut self, paddr: u64, val: u64) {
        self.write_physical(paddr, &val.to_le_bytes());
    }

    fn write_u128(&mut self, paddr: u64, val: u128) {
        self.write_physical(paddr, &val.to_le_bytes());
    }

    // Aliases matching the project documentation (physical accessors).
    fn read_physical_u8(&mut self, paddr: u64) -> u8 {
        self.read_u8(paddr)
    }

    fn read_physical_u16(&mut self, paddr: u64) -> u16 {
        self.read_u16(paddr)
    }

    fn read_physical_u32(&mut self, paddr: u64) -> u32 {
        self.read_u32(paddr)
    }

    fn read_physical_u64(&mut self, paddr: u64) -> u64 {
        self.read_u64(paddr)
    }

    fn read_physical_u128(&mut self, paddr: u64) -> u128 {
        self.read_u128(paddr)
    }

    fn write_physical_u8(&mut self, paddr: u64, val: u8) {
        self.write_u8(paddr, val);
    }

    fn write_physical_u16(&mut self, paddr: u64, val: u16) {
        self.write_u16(paddr, val);
    }

    fn write_physical_u32(&mut self, paddr: u64, val: u32) {
        self.write_u32(paddr, val);
    }

    fn write_physical_u64(&mut self, paddr: u64, val: u64) {
        self.write_u64(paddr, val);
    }

    fn write_physical_u128(&mut self, paddr: u64, val: u128) {
        self.write_u128(paddr, val);
    }
}

impl<T: MemoryBus + ?Sized> MemoryBus for &mut T {
    #[inline]
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        <T as MemoryBus>::read_physical(&mut **self, paddr, buf)
    }

    #[inline]
    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        <T as MemoryBus>::write_physical(&mut **self, paddr, buf)
    }

    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        <T as MemoryBus>::read_u8(&mut **self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        <T as MemoryBus>::read_u16(&mut **self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        <T as MemoryBus>::read_u32(&mut **self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        <T as MemoryBus>::read_u64(&mut **self, paddr)
    }

    #[inline]
    fn read_u128(&mut self, paddr: u64) -> u128 {
        <T as MemoryBus>::read_u128(&mut **self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, val: u8) {
        <T as MemoryBus>::write_u8(&mut **self, paddr, val)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, val: u16) {
        <T as MemoryBus>::write_u16(&mut **self, paddr, val)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, val: u32) {
        <T as MemoryBus>::write_u32(&mut **self, paddr, val)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, val: u64) {
        <T as MemoryBus>::write_u64(&mut **self, paddr, val)
    }

    #[inline]
    fn write_u128(&mut self, paddr: u64, val: u128) {
        <T as MemoryBus>::write_u128(&mut **self, paddr, val)
    }
}

#[derive(Debug, Clone)]
pub struct RomRegion {
    pub start: u64,
    pub data: Arc<[u8]>,
}

impl RomRegion {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn end(&self) -> u64 {
        self.start.saturating_add(self.len())
    }
}

pub struct MmioRegion {
    pub start: u64,
    pub end: u64,
    pub handler: Box<dyn MmioHandler>,
}

impl MmioRegion {
    fn contains(&self, addr: u64) -> bool {
        addr >= self.start && addr < self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    AddressOverflow,
    Overlap,
}

/// Guest *physical* memory bus.
///
/// Routes accesses to:
/// 1. MMIO regions (highest priority)
/// 2. ROM regions
/// 3. RAM
/// 4. Unmapped (reads return all 1s, writes ignored)
pub struct PhysicalMemoryBus {
    pub ram: Box<dyn GuestMemory>,
    rom_regions: Vec<RomRegion>,
    mmio_regions: Vec<MmioRegion>,
}

impl PhysicalMemoryBus {
    pub fn new(ram: Box<dyn GuestMemory>) -> Self {
        Self {
            ram,
            rom_regions: Vec::new(),
            mmio_regions: Vec::new(),
        }
    }

    pub fn rom_regions(&self) -> &[RomRegion] {
        &self.rom_regions
    }

    pub fn mmio_regions(&self) -> &[MmioRegion] {
        &self.mmio_regions
    }

    pub fn map_rom(&mut self, start: u64, data: Arc<[u8]>) -> Result<(), MapError> {
        let len = data.len() as u64;
        let end = start.checked_add(len).ok_or(MapError::AddressOverflow)?;

        let insert_idx = self.rom_regions.partition_point(|r| r.start < start);

        if let Some(prev) = insert_idx
            .checked_sub(1)
            .and_then(|idx| self.rom_regions.get(idx))
        {
            if prev.end() > start {
                return Err(MapError::Overlap);
            }
        }

        if let Some(next) = self.rom_regions.get(insert_idx) {
            if end > next.start {
                return Err(MapError::Overlap);
            }
        }

        self.rom_regions
            .insert(insert_idx, RomRegion { start, data });
        Ok(())
    }

    pub fn map_mmio(
        &mut self,
        start: u64,
        len: u64,
        handler: Box<dyn MmioHandler>,
    ) -> Result<(), MapError> {
        let end = start.checked_add(len).ok_or(MapError::AddressOverflow)?;

        let insert_idx = self.mmio_regions.partition_point(|r| r.start < start);

        if let Some(prev) = insert_idx
            .checked_sub(1)
            .and_then(|idx| self.mmio_regions.get(idx))
        {
            if prev.end > start {
                return Err(MapError::Overlap);
            }
        }

        if let Some(next) = self.mmio_regions.get(insert_idx) {
            if end > next.start {
                return Err(MapError::Overlap);
            }
        }

        self.mmio_regions.insert(
            insert_idx,
            MmioRegion {
                start,
                end,
                handler,
            },
        );
        Ok(())
    }

    fn find_mmio_region_index(&self, addr: u64) -> Option<usize> {
        let idx = self.mmio_regions.partition_point(|r| r.start <= addr);
        let idx = idx.checked_sub(1)?;
        self.mmio_regions
            .get(idx)
            .is_some_and(|r| r.contains(addr))
            .then_some(idx)
    }

    fn find_rom_region_index(&self, addr: u64) -> Option<usize> {
        let idx = self.rom_regions.partition_point(|r| r.start <= addr);
        let idx = idx.checked_sub(1)?;
        self.rom_regions
            .get(idx)
            .is_some_and(|r| addr >= r.start && addr < r.end())
            .then_some(idx)
    }

    fn next_mmio_start_after(&self, addr: u64) -> Option<u64> {
        let idx = self.mmio_regions.partition_point(|r| r.start <= addr);
        self.mmio_regions.get(idx).map(|r| r.start)
    }

    fn next_rom_start_after(&self, addr: u64) -> Option<u64> {
        let idx = self.rom_regions.partition_point(|r| r.start <= addr);
        self.rom_regions.get(idx).map(|r| r.start)
    }

    pub fn read_physical(&mut self, paddr: u64, dst: &mut [u8]) {
        let mut pos = 0usize;
        let ram_len = self.ram.size();

        while pos < dst.len() {
            let addr = match paddr.checked_add(pos as u64) {
                Some(v) => v,
                None => {
                    dst[pos..].fill(0xFF);
                    break;
                }
            };

            if let Some(mmio_idx) = self.find_mmio_region_index(addr) {
                let (region_start, region_end) = {
                    let r = &self.mmio_regions[mmio_idx];
                    (r.start, r.end)
                };
                let rem = dst.len() - pos;
                let chunk_end = region_end.min(addr.saturating_add(rem as u64));
                let chunk_len = (chunk_end - addr) as usize;

                let dst_chunk = &mut dst[pos..pos + chunk_len];
                self.read_mmio_chunk(mmio_idx, addr - region_start, dst_chunk);
                pos += chunk_len;
                continue;
            }

            if let Some(rom_idx) = self.find_rom_region_index(addr) {
                let (rom_start, rom_end, rom_data) = {
                    let r = &self.rom_regions[rom_idx];
                    (r.start, r.end(), Arc::clone(&r.data))
                };

                let mut chunk_end = rom_end;
                if let Some(next_mmio) = self.next_mmio_start_after(addr) {
                    if next_mmio < chunk_end {
                        chunk_end = next_mmio;
                    }
                }

                let rem = dst.len() - pos;
                let chunk_end = chunk_end.min(addr.saturating_add(rem as u64));
                let chunk_len = (chunk_end - addr) as usize;

                let src_off = (addr - rom_start) as usize;
                let src = &rom_data[src_off..src_off + chunk_len];
                dst[pos..pos + chunk_len].copy_from_slice(src);
                pos += chunk_len;
                continue;
            }

            if addr < ram_len {
                let mut chunk_end = ram_len;
                if let Some(next_mmio) = self.next_mmio_start_after(addr) {
                    if next_mmio < chunk_end {
                        chunk_end = next_mmio;
                    }
                }
                if let Some(next_rom) = self.next_rom_start_after(addr) {
                    if next_rom < chunk_end {
                        chunk_end = next_rom;
                    }
                }

                let rem = dst.len() - pos;
                let chunk_end = chunk_end.min(addr.saturating_add(rem as u64));
                let chunk_len = (chunk_end - addr) as usize;

                if self
                    .ram
                    .read_into(addr, &mut dst[pos..pos + chunk_len])
                    .is_err()
                {
                    dst[pos..pos + chunk_len].fill(0xFF);
                }
                pos += chunk_len;
                continue;
            }

            // Unmapped: return all 1s for the requested width.
            let mut chunk_end = None;
            if let Some(next_mmio) = self.next_mmio_start_after(addr) {
                chunk_end = Some(next_mmio);
            }
            if let Some(next_rom) = self.next_rom_start_after(addr) {
                chunk_end = Some(match chunk_end {
                    Some(existing) => existing.min(next_rom),
                    None => next_rom,
                });
            }

            let rem = dst.len() - pos;
            let chunk_len = match chunk_end {
                Some(end) => {
                    let diff = end.saturating_sub(addr);
                    diff.min(rem as u64) as usize
                }
                None => rem,
            };

            dst[pos..pos + chunk_len].fill(0xFF);
            pos += chunk_len;
        }
    }

    pub fn write_physical(&mut self, paddr: u64, src: &[u8]) {
        let mut pos = 0usize;
        let ram_len = self.ram.size();

        while pos < src.len() {
            let addr = match paddr.checked_add(pos as u64) {
                Some(v) => v,
                None => break,
            };

            if let Some(mmio_idx) = self.find_mmio_region_index(addr) {
                let (region_start, region_end) = {
                    let r = &self.mmio_regions[mmio_idx];
                    (r.start, r.end)
                };

                let rem = src.len() - pos;
                let chunk_end = region_end.min(addr.saturating_add(rem as u64));
                let chunk_len = (chunk_end - addr) as usize;

                let src_chunk = &src[pos..pos + chunk_len];
                self.write_mmio_chunk(mmio_idx, addr - region_start, src_chunk);
                pos += chunk_len;
                continue;
            }

            if let Some(rom_idx) = self.find_rom_region_index(addr) {
                let rom_end = self.rom_regions[rom_idx].end();

                let mut chunk_end = rom_end;
                if let Some(next_mmio) = self.next_mmio_start_after(addr) {
                    if next_mmio < chunk_end {
                        chunk_end = next_mmio;
                    }
                }

                let rem = src.len() - pos;
                let chunk_end = chunk_end.min(addr.saturating_add(rem as u64));
                let chunk_len = (chunk_end - addr) as usize;

                // ROM is read-only: ignore writes.
                pos += chunk_len;
                continue;
            }

            if addr < ram_len {
                let mut chunk_end = ram_len;
                if let Some(next_mmio) = self.next_mmio_start_after(addr) {
                    if next_mmio < chunk_end {
                        chunk_end = next_mmio;
                    }
                }
                if let Some(next_rom) = self.next_rom_start_after(addr) {
                    if next_rom < chunk_end {
                        chunk_end = next_rom;
                    }
                }

                let rem = src.len() - pos;
                let chunk_end = chunk_end.min(addr.saturating_add(rem as u64));
                let chunk_len = (chunk_end - addr) as usize;

                let _ = self.ram.write_from(addr, &src[pos..pos + chunk_len]);
                pos += chunk_len;
                continue;
            }

            // Unmapped: writes are ignored. Skip until the next mapped region.
            let mut chunk_end = None;
            if let Some(next_mmio) = self.next_mmio_start_after(addr) {
                chunk_end = Some(next_mmio);
            }
            if let Some(next_rom) = self.next_rom_start_after(addr) {
                chunk_end = Some(match chunk_end {
                    Some(existing) => existing.min(next_rom),
                    None => next_rom,
                });
            }

            let rem = src.len() - pos;
            let chunk_len = match chunk_end {
                Some(end) => {
                    let diff = end.saturating_sub(addr);
                    diff.min(rem as u64) as usize
                }
                None => rem,
            };

            pos += chunk_len;
        }
    }

    fn read_mmio_chunk(&mut self, region_idx: usize, offset: u64, dst: &mut [u8]) {
        let mut pos = 0usize;
        while pos < dst.len() {
            let addr = offset.wrapping_add(pos as u64);
            let remaining = dst.len() - pos;
            // Use naturally-aligned access sizes to avoid issuing unaligned 64-bit MMIO operations
            // to device models that require alignment (e.g. HPET 64-bit registers).
            //
            // This also keeps the access sizes in the common PCI/MMIO set {1,2,4,8} even when
            // higher-level callers request an arbitrary byte count (e.g. DMA-style reads).
            let size = [8usize, 4, 2, 1]
                .into_iter()
                .find(|&candidate| remaining >= candidate && addr.is_multiple_of(candidate as u64))
                .unwrap_or(1);
            let value = self.mmio_regions[region_idx].handler.read(addr, size);
            let bytes = value.to_le_bytes();
            dst[pos..pos + size].copy_from_slice(&bytes[..size]);
            pos += size;
        }
    }

    fn write_mmio_chunk(&mut self, region_idx: usize, offset: u64, src: &[u8]) {
        let mut pos = 0usize;
        while pos < src.len() {
            let addr = offset.wrapping_add(pos as u64);
            let remaining = src.len() - pos;
            let size = [8usize, 4, 2, 1]
                .into_iter()
                .find(|&candidate| remaining >= candidate && addr.is_multiple_of(candidate as u64))
                .unwrap_or(1);
            let mut buf = [0u8; 8];
            buf[..size].copy_from_slice(&src[pos..pos + size]);
            let value = u64::from_le_bytes(buf);
            self.mmio_regions[region_idx]
                .handler
                .write(addr, size, value);
            pos += size;
        }
    }

    pub fn read_physical_u8(&mut self, paddr: u64) -> u8 {
        let mut buf = [0u8; 1];
        self.read_physical(paddr, &mut buf);
        buf[0]
    }

    pub fn read_physical_u16(&mut self, paddr: u64) -> u16 {
        let mut buf = [0u8; 2];
        self.read_physical(paddr, &mut buf);
        u16::from_le_bytes(buf)
    }

    pub fn read_physical_u32(&mut self, paddr: u64) -> u32 {
        let mut buf = [0u8; 4];
        self.read_physical(paddr, &mut buf);
        u32::from_le_bytes(buf)
    }

    pub fn read_physical_u64(&mut self, paddr: u64) -> u64 {
        let mut buf = [0u8; 8];
        self.read_physical(paddr, &mut buf);
        u64::from_le_bytes(buf)
    }

    pub fn read_physical_u128(&mut self, paddr: u64) -> u128 {
        let mut buf = [0u8; 16];
        self.read_physical(paddr, &mut buf);
        u128::from_le_bytes(buf)
    }

    pub fn write_physical_u8(&mut self, paddr: u64, value: u8) {
        self.write_physical(paddr, &[value]);
    }

    pub fn write_physical_u16(&mut self, paddr: u64, value: u16) {
        self.write_physical(paddr, &value.to_le_bytes());
    }

    pub fn write_physical_u32(&mut self, paddr: u64, value: u32) {
        self.write_physical(paddr, &value.to_le_bytes());
    }

    pub fn write_physical_u64(&mut self, paddr: u64, value: u64) {
        self.write_physical(paddr, &value.to_le_bytes());
    }

    pub fn write_physical_u128(&mut self, paddr: u64, value: u128) {
        self.write_physical(paddr, &value.to_le_bytes());
    }
}

impl MemoryBus for PhysicalMemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        PhysicalMemoryBus::read_physical(self, paddr, buf)
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        PhysicalMemoryBus::write_physical(self, paddr, buf)
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::phys::{GuestMemoryError, GuestMemoryResult};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct SharedRam {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedRam {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes: Arc::new(Mutex::new(bytes)),
            }
        }

        fn snapshot(&self) -> Vec<u8> {
            self.bytes.lock().unwrap().clone()
        }
    }

    impl GuestMemory for SharedRam {
        fn size(&self) -> u64 {
            self.bytes.lock().unwrap().len() as u64
        }

        fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
            let bytes = self.bytes.lock().unwrap();
            let size = bytes.len() as u64;
            let len = dst.len();

            let end = paddr
                .checked_add(len as u64)
                .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
            if end > size {
                return Err(GuestMemoryError::OutOfRange { paddr, len, size });
            }

            let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
                paddr,
                len,
                size,
            })?;
            let end =
                start
                    .checked_add(len)
                    .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;

            dst.copy_from_slice(&bytes[start..end]);
            Ok(())
        }

        fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
            let mut bytes = self.bytes.lock().unwrap();
            let size = bytes.len() as u64;
            let len = src.len();

            let end = paddr
                .checked_add(len as u64)
                .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
            if end > size {
                return Err(GuestMemoryError::OutOfRange { paddr, len, size });
            }

            let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
                paddr,
                len,
                size,
            })?;
            let end =
                start
                    .checked_add(len)
                    .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;

            bytes[start..end].copy_from_slice(src);
            Ok(())
        }
    }

    #[derive(Default)]
    struct MmioState {
        mem: Vec<u8>,
        reads: Vec<(u64, usize)>,
        writes: Vec<(u64, usize, u64)>,
    }

    #[derive(Clone)]
    struct RecordingMmio {
        state: Arc<Mutex<MmioState>>,
    }

    impl RecordingMmio {
        fn new(mem: Vec<u8>) -> (Self, Arc<Mutex<MmioState>>) {
            let state = Arc::new(Mutex::new(MmioState {
                mem,
                ..Default::default()
            }));
            (
                Self {
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl MmioHandler for RecordingMmio {
        fn read(&mut self, offset: u64, size: usize) -> u64 {
            let mut state = self.state.lock().unwrap();
            state.reads.push((offset, size));
            let mut buf = [0xFFu8; 8];
            let off = offset as usize;
            for (i, byte) in buf.iter_mut().enumerate().take(size.min(8)) {
                *byte = state.mem.get(off + i).copied().unwrap_or(0xFF);
            }
            u64::from_le_bytes(buf)
        }

        fn write(&mut self, offset: u64, size: usize, value: u64) {
            let mut state = self.state.lock().unwrap();
            state.writes.push((offset, size, value));
            let bytes = value.to_le_bytes();
            let off = offset as usize;
            for (i, byte) in bytes.iter().enumerate().take(size.min(8)) {
                if let Some(dst) = state.mem.get_mut(off + i) {
                    *dst = *byte;
                }
            }
        }
    }

    #[derive(Default)]
    struct StrictMmioState {
        mem: Vec<u8>,
        reads: Vec<(u64, usize)>,
        writes: Vec<(u64, usize, u64)>,
    }

    #[derive(Clone)]
    struct StrictMmio {
        state: Arc<Mutex<StrictMmioState>>,
    }

    impl StrictMmio {
        fn new(mem: Vec<u8>) -> (Self, Arc<Mutex<StrictMmioState>>) {
            let state = Arc::new(Mutex::new(StrictMmioState {
                mem,
                ..Default::default()
            }));
            (
                Self {
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl MmioHandler for StrictMmio {
        fn read(&mut self, offset: u64, size: usize) -> u64 {
            assert!(
                matches!(size, 1 | 2 | 4 | 8),
                "unexpected MMIO read size {size}"
            );
            assert_eq!(
                offset % size as u64,
                0,
                "unaligned MMIO read: offset={offset} size={size}"
            );

            let mut state = self.state.lock().unwrap();
            state.reads.push((offset, size));

            let mut buf = [0xFFu8; 8];
            let off = offset as usize;
            for (i, byte) in buf.iter_mut().enumerate().take(size) {
                *byte = state.mem.get(off + i).copied().unwrap_or(0xFF);
            }
            u64::from_le_bytes(buf)
        }

        fn write(&mut self, offset: u64, size: usize, value: u64) {
            assert!(
                matches!(size, 1 | 2 | 4 | 8),
                "unexpected MMIO write size {size}"
            );
            assert_eq!(
                offset % size as u64,
                0,
                "unaligned MMIO write: offset={offset} size={size}"
            );

            let mut state = self.state.lock().unwrap();
            state.writes.push((offset, size, value));
            let bytes = value.to_le_bytes();
            let off = offset as usize;
            for (i, byte) in bytes.iter().enumerate().take(size) {
                if let Some(dst) = state.mem.get_mut(off + i) {
                    *dst = *byte;
                }
            }
        }
    }

    #[test]
    fn unmapped_reads_return_all_ones() {
        let ram = SharedRam::new(vec![0u8; 4]);
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        assert_eq!(bus.read_physical_u8(10), 0xFF);
        assert_eq!(bus.read_physical_u16(10), 0xFFFF);
        assert_eq!(bus.read_physical_u32(10), 0xFFFF_FFFF);
        assert_eq!(bus.read_physical_u64(10), 0xFFFF_FFFF_FFFF_FFFF);
        assert_eq!(bus.read_physical_u128(10), u128::MAX);

        let mut buf = [0u8; 3];
        bus.read_physical(10, &mut buf);
        assert_eq!(buf, [0xFF; 3]);
    }

    #[test]
    fn rom_is_read_only_and_does_not_write_through_to_ram() {
        let ram = SharedRam::new((0u8..16).collect());
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        bus.map_rom(4, Arc::from([0x10u8, 0x11, 0x12, 0x13].as_slice()))
            .unwrap();

        assert_eq!(bus.read_physical_u32(4), 0x1312_1110);

        bus.write_physical_u32(4, 0xAABB_CCDD);

        // ROM view is unchanged.
        assert_eq!(bus.read_physical_u32(4), 0x1312_1110);

        // Underlying RAM is unchanged too.
        let snap = ram_view.snapshot();
        assert_eq!(&snap[4..8], &[4u8, 5, 6, 7]);
    }

    #[test]
    fn mmio_overrides_rom_and_ram() {
        let mut base_ram: Vec<u8> = vec![0; 32];
        base_ram[8..12].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        let ram = SharedRam::new(base_ram);
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        bus.map_rom(8, Arc::from([0x10u8, 0x20, 0x30, 0x40].as_slice()))
            .unwrap();

        let (mmio, mmio_state) = RecordingMmio::new(vec![0xAA, 0xBB, 0xCC, 0xDD]);
        bus.map_mmio(8, 4, Box::new(mmio)).unwrap();

        assert_eq!(bus.read_physical_u32(8), 0xDDCC_BBAA);

        bus.write_physical_u32(8, 0x1122_3344);

        let state = mmio_state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.writes[0].0, 0);
        assert_eq!(state.writes[0].1, 4);
        assert_eq!(state.mem, vec![0x44, 0x33, 0x22, 0x11]);
        drop(state);

        // RAM was not modified by the MMIO write.
        let snap = ram_view.snapshot();
        assert_eq!(&snap[8..12], &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn cross_boundary_accesses_are_split_safely() {
        // Crossing the end of RAM should not panic and should use 0xFF for unmapped bytes.
        let ram = SharedRam::new(vec![0x01, 0x02, 0x03, 0x04]);
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        assert_eq!(bus.read_physical_u32(2), 0xFFFF_0403);

        bus.write_physical_u32(2, 0xAABB_CCDD);
        let snap = ram_view.snapshot();
        assert_eq!(snap, vec![0x01, 0x02, 0xDD, 0xCC]);
        assert_eq!(bus.read_physical_u32(2), 0xFFFF_CCDD);

        // Crossing into MMIO: RAM[3], MMIO[0..2], RAM[6]
        let base_ram: Vec<u8> = (0u8..8).collect();
        let ram = SharedRam::new(base_ram.clone());
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        let (mmio, mmio_state) = RecordingMmio::new(vec![0xAA, 0xBB]);
        bus.map_mmio(4, 2, Box::new(mmio)).unwrap();

        let v = bus.read_physical_u32(3);
        assert_eq!(v, u32::from_le_bytes([3, 0xAA, 0xBB, 6]));

        bus.write_physical_u32(3, u32::from_le_bytes([0x11, 0x22, 0x33, 0x44]));

        let snap = ram_view.snapshot();
        assert_eq!(snap[3], 0x11);
        assert_eq!(snap[6], 0x44);

        let state = mmio_state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.writes[0].0, 0);
        assert_eq!(state.writes[0].1, 2);
        assert_eq!(state.mem, vec![0x22, 0x33]);
    }

    #[test]
    fn unaligned_u64_mmio_accesses_are_split_into_aligned_operations() {
        let ram = SharedRam::new(vec![0u8; 32]);
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        let (mmio, mmio_state) = StrictMmio::new((0u8..16).collect());
        bus.map_mmio(0x1000, 16, Box::new(mmio)).unwrap();

        // This access starts at an odd offset within the MMIO region; the bus must not issue an
        // unaligned 8-byte read to the handler.
        let got = bus.read_physical_u64(0x1001);
        assert_eq!(got, u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]));

        bus.write_physical_u64(0x1001, 0x1122_3344_5566_7788);

        let state = mmio_state.lock().unwrap();
        assert_eq!(
            &state.mem[1..9],
            &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
    }

    #[test]
    fn small_bus_avoids_unaligned_multi_byte_mmio_operations() {
        // The lightweight `Bus` router provides an 8-byte fast path for MMIO reads/writes. Ensure
        // it doesn't invoke unaligned multi-byte operations when the access is unaligned.
        let mut bus = Bus::new(0);

        let (mmio, mmio_state) = StrictMmio::new((0u8..16).collect());
        bus.map_mmio(0x1000, 16, Box::new(mmio));

        let got = bus.read(0x1001, 8);
        assert_eq!(got, u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]));

        bus.write(0x1001, 8, 0x1122_3344_5566_7788);

        let state = mmio_state.lock().unwrap();
        assert_eq!(
            &state.mem[1..9],
            &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
    }
}

/// Simple MMIO handler interface for [`Bus`].
pub trait MmioHandler {
    fn read(&mut self, offset: u64, size: usize) -> u64;
    fn write(&mut self, offset: u64, size: usize, value: u64);
}

enum RegionKind {
    Rom(Box<[u8]>),
    Mmio(Box<dyn MmioHandler>),
}

struct Region {
    start: u64,
    end: u64,
    kind: RegionKind,
}

impl Region {
    fn contains(&self, addr: u64) -> bool {
        self.start <= addr && addr < self.end
    }
}

/// A small physical address space router supporting RAM, ROM, and MMIO mappings.
///
/// This is primarily intended for fuzzing and unit tests. Out-of-range reads return `0xff` and
/// writes to unmapped/ROM regions are ignored to guarantee deterministic behavior.
pub struct Bus {
    ram: Vec<u8>,
    regions: Vec<Region>,
}

fn mask_for_size(size: usize) -> u64 {
    match size {
        0 => 0,
        1 => 0xff,
        2 => 0xffff,
        3 => 0x00ff_ffff,
        4 => 0xffff_ffff,
        5 => 0x0000_ffff_ffff,
        6 => 0x00ff_ffff_ffff,
        7 => 0x00ff_ffff_ffff_ffff,
        _ => u64::MAX,
    }
}

impl Bus {
    pub fn new(ram_size: usize) -> Self {
        Self {
            ram: vec![0; ram_size],
            regions: Vec::new(),
        }
    }

    pub fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.ram
    }

    pub fn map_rom(&mut self, start: u64, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }

        let len = data.len() as u64;
        let Some(end) = start.checked_add(len) else {
            return;
        };

        self.regions.push(Region {
            start,
            end,
            kind: RegionKind::Rom(data.into_boxed_slice()),
        });
    }

    pub fn map_mmio(&mut self, start: u64, len: u64, handler: Box<dyn MmioHandler>) {
        if len == 0 {
            return;
        }
        let Some(end) = start.checked_add(len) else {
            return;
        };

        self.regions.push(Region {
            start,
            end,
            kind: RegionKind::Mmio(handler),
        });
    }

    fn find_region_index(&self, addr: u64) -> Option<usize> {
        // MMIO always takes precedence over ROM (and RAM). Within each region kind,
        // the last mapping wins to keep behavior deterministic even with overlaps.
        self.regions
            .iter()
            .rposition(|r| matches!(&r.kind, RegionKind::Mmio(_)) && r.contains(addr))
            .or_else(|| {
                self.regions
                    .iter()
                    .rposition(|r| matches!(&r.kind, RegionKind::Rom(_)) && r.contains(addr))
            })
    }

    /// Reads up to 8 bytes little-endian from the bus.
    pub fn read(&mut self, addr: u64, size: usize) -> u64 {
        if !(1..=8).contains(&size) {
            return 0;
        }

        // Fast path for single-region MMIO/ROM reads that fit in one handler call. This avoids
        // breaking devices like HPET whose registers have side effects on each write.
        if matches!(size, 1 | 2 | 4 | 8) {
            if let Some(region_idx) = self.find_region_index(addr) {
                let last = addr.saturating_add(size.saturating_sub(1) as u64);
                if self.regions[region_idx].contains(last) {
                    let region = &mut self.regions[region_idx];
                    let offset = addr - region.start;
                    // Only issue multi-byte MMIO/ROM accesses when naturally aligned. Unaligned
                    // reads fall back to byte-granular operations to avoid sending unaligned
                    // 64-bit operations to device models that require alignment.
                    if !offset.is_multiple_of(size as u64) {
                        // Fall through to byte-wise reads below.
                    } else {
                        return match &mut region.kind {
                            RegionKind::Mmio(handler) => {
                                handler.read(offset, size) & mask_for_size(size)
                            }
                            RegionKind::Rom(bytes) => {
                                let base = offset as usize;
                                let mut out = 0u64;
                                for i in 0..size {
                                    let byte = bytes.get(base + i).copied().unwrap_or(0xff);
                                    out |= (byte as u64) << (i * 8);
                                }
                                out
                            }
                        };
                    }
                }
            }
        }

        let mut out = 0u64;
        for i in 0..size {
            let byte = self.read_u8(addr.wrapping_add(i as u64));
            out |= (byte as u64) << (i * 8);
        }
        out
    }

    /// Writes up to 8 bytes little-endian to the bus.
    pub fn write(&mut self, addr: u64, size: usize, value: u64) {
        if !(1..=8).contains(&size) {
            return;
        }

        // Fast path for single-region MMIO writes. This keeps multi-byte register writes atomic
        // from the device model's perspective, which is important for comparator-style devices
        // like HPET.
        if matches!(size, 1 | 2 | 4 | 8) {
            if let Some(region_idx) = self.find_region_index(addr) {
                let last = addr.saturating_add(size.saturating_sub(1) as u64);
                if self.regions[region_idx].contains(last) {
                    let region = &mut self.regions[region_idx];
                    let offset = addr - region.start;
                    if !offset.is_multiple_of(size as u64) {
                        // Fall through to byte-wise writes below.
                    } else {
                        match &mut region.kind {
                            RegionKind::Rom(_) => {}
                            RegionKind::Mmio(handler) => {
                                handler.write(offset, size, value & mask_for_size(size));
                            }
                        }
                        return;
                    }
                }
            }
        }

        for i in 0..size {
            let byte = ((value >> (i * 8)) & 0xff) as u8;
            self.write_u8(addr.wrapping_add(i as u64), byte);
        }
    }

    fn read_u8(&mut self, addr: u64) -> u8 {
        if let Some(region_idx) = self.find_region_index(addr) {
            let region = &mut self.regions[region_idx];
            let offset = addr - region.start;
            return match &mut region.kind {
                RegionKind::Rom(bytes) => bytes.get(offset as usize).copied().unwrap_or(0xff),
                RegionKind::Mmio(handler) => handler.read(offset, 1) as u8,
            };
        }

        let Ok(idx) = usize::try_from(addr) else {
            return 0xff;
        };
        self.ram.get(idx).copied().unwrap_or(0xff)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        if let Some(region_idx) = self.find_region_index(addr) {
            let region = &mut self.regions[region_idx];
            let offset = addr - region.start;
            match &mut region.kind {
                RegionKind::Rom(_) => {}
                RegionKind::Mmio(handler) => handler.write(offset, 1, value as u64),
            }
            return;
        }

        let Ok(idx) = usize::try_from(addr) else {
            return;
        };
        if let Some(slot) = self.ram.get_mut(idx) {
            *slot = value;
        }
    }
}

impl MemoryBus for Bus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = self.read_u8(paddr.wrapping_add(i as u64));
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, byte) in buf.iter().enumerate() {
            self.write_u8(paddr.wrapping_add(i as u64), *byte);
        }
    }
}

#[cfg(test)]
mod bus_tests {
    use super::*;
    use crate::phys::{GuestMemoryError, GuestMemoryResult};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct SharedRam {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedRam {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes: Arc::new(Mutex::new(bytes)),
            }
        }

        fn snapshot(&self) -> Vec<u8> {
            self.bytes.lock().unwrap().clone()
        }
    }

    impl GuestMemory for SharedRam {
        fn size(&self) -> u64 {
            self.bytes.lock().unwrap().len() as u64
        }

        fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
            let bytes = self.bytes.lock().unwrap();
            let size = bytes.len() as u64;
            let len = dst.len();

            let end = paddr
                .checked_add(len as u64)
                .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
            if end > size {
                return Err(GuestMemoryError::OutOfRange { paddr, len, size });
            }

            let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
                paddr,
                len,
                size,
            })?;
            let end =
                start
                    .checked_add(len)
                    .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;

            dst.copy_from_slice(&bytes[start..end]);
            Ok(())
        }

        fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
            let mut bytes = self.bytes.lock().unwrap();
            let size = bytes.len() as u64;
            let len = src.len();

            let end = paddr
                .checked_add(len as u64)
                .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
            if end > size {
                return Err(GuestMemoryError::OutOfRange { paddr, len, size });
            }

            let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
                paddr,
                len,
                size,
            })?;
            let end =
                start
                    .checked_add(len)
                    .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;

            bytes[start..end].copy_from_slice(src);
            Ok(())
        }
    }

    #[derive(Default)]
    struct MmioState {
        mem: Vec<u8>,
        reads: Vec<(u64, usize)>,
        writes: Vec<(u64, usize, u64)>,
    }

    #[derive(Clone)]
    struct RecordingMmio {
        state: Arc<Mutex<MmioState>>,
    }

    impl RecordingMmio {
        fn new(mem: Vec<u8>) -> (Self, Arc<Mutex<MmioState>>) {
            let state = Arc::new(Mutex::new(MmioState {
                mem,
                ..Default::default()
            }));
            (
                Self {
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl MmioHandler for RecordingMmio {
        fn read(&mut self, offset: u64, size: usize) -> u64 {
            let mut state = self.state.lock().unwrap();
            state.reads.push((offset, size));
            let mut buf = [0xFFu8; 8];
            let off = offset as usize;
            for (i, slot) in buf.iter_mut().take(size.min(8)).enumerate() {
                *slot = state.mem.get(off + i).copied().unwrap_or(0xFF);
            }
            u64::from_le_bytes(buf)
        }

        fn write(&mut self, offset: u64, size: usize, value: u64) {
            let mut state = self.state.lock().unwrap();
            state.writes.push((offset, size, value));
            let bytes = value.to_le_bytes();
            let off = offset as usize;
            for (i, byte) in bytes.iter().copied().take(size.min(8)).enumerate() {
                if let Some(dst) = state.mem.get_mut(off + i) {
                    *dst = byte;
                }
            }
        }
    }

    #[test]
    fn unmapped_reads_return_all_ones() {
        let ram = SharedRam::new(vec![0u8; 4]);
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        assert_eq!(bus.read_physical_u8(10), 0xFF);
        assert_eq!(bus.read_physical_u16(10), 0xFFFF);
        assert_eq!(bus.read_physical_u32(10), 0xFFFF_FFFF);
        assert_eq!(bus.read_physical_u64(10), 0xFFFF_FFFF_FFFF_FFFF);
        assert_eq!(bus.read_physical_u128(10), u128::MAX);

        let mut buf = [0u8; 3];
        bus.read_physical(10, &mut buf);
        assert_eq!(buf, [0xFF; 3]);
    }

    #[test]
    fn rom_is_read_only_and_does_not_write_through_to_ram() {
        let ram = SharedRam::new((0u8..16).collect());
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        bus.map_rom(4, Arc::from([0x10u8, 0x11, 0x12, 0x13].as_slice()))
            .unwrap();

        assert_eq!(bus.read_physical_u32(4), 0x1312_1110);

        bus.write_physical_u32(4, 0xAABB_CCDD);

        // ROM view is unchanged.
        assert_eq!(bus.read_physical_u32(4), 0x1312_1110);

        // Underlying RAM is unchanged too.
        let snap = ram_view.snapshot();
        assert_eq!(&snap[4..8], &[4u8, 5, 6, 7]);
    }

    #[test]
    fn mmio_overrides_rom_and_ram() {
        let mut base_ram: Vec<u8> = vec![0; 32];
        base_ram[8..12].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        let ram = SharedRam::new(base_ram);
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        bus.map_rom(8, Arc::from([0x10u8, 0x20, 0x30, 0x40].as_slice()))
            .unwrap();

        let (mmio, mmio_state) = RecordingMmio::new(vec![0xAA, 0xBB, 0xCC, 0xDD]);
        bus.map_mmio(8, 4, Box::new(mmio)).unwrap();

        assert_eq!(bus.read_physical_u32(8), 0xDDCC_BBAA);

        bus.write_physical_u32(8, 0x1122_3344);

        let state = mmio_state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.writes[0].0, 0);
        assert_eq!(state.writes[0].1, 4);
        assert_eq!(state.mem, vec![0x44, 0x33, 0x22, 0x11]);
        drop(state);

        // RAM was not modified by the MMIO write.
        let snap = ram_view.snapshot();
        assert_eq!(&snap[8..12], &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn cross_boundary_accesses_are_split_safely() {
        // Crossing the end of RAM should not panic and should use 0xFF for unmapped bytes.
        let ram = SharedRam::new(vec![0x01, 0x02, 0x03, 0x04]);
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        assert_eq!(bus.read_physical_u32(2), 0xFFFF_0403);

        bus.write_physical_u32(2, 0xAABB_CCDD);
        let snap = ram_view.snapshot();
        assert_eq!(snap, vec![0x01, 0x02, 0xDD, 0xCC]);
        assert_eq!(bus.read_physical_u32(2), 0xFFFF_CCDD);

        // Crossing into MMIO: RAM[3], MMIO[0..2], RAM[6]
        let base_ram: Vec<u8> = (0u8..8).collect();
        let ram = SharedRam::new(base_ram.clone());
        let ram_view = ram.clone();
        let mut bus = PhysicalMemoryBus::new(Box::new(ram));

        let (mmio, mmio_state) = RecordingMmio::new(vec![0xAA, 0xBB]);
        bus.map_mmio(4, 2, Box::new(mmio)).unwrap();

        let v = bus.read_physical_u32(3);
        assert_eq!(v, u32::from_le_bytes([3, 0xAA, 0xBB, 6]));

        bus.write_physical_u32(3, u32::from_le_bytes([0x11, 0x22, 0x33, 0x44]));

        let snap = ram_view.snapshot();
        assert_eq!(snap[3], 0x11);
        assert_eq!(snap[6], 0x44);

        let state = mmio_state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.writes[0].0, 0);
        assert_eq!(state.writes[0].1, 2);
        assert_eq!(state.mem, vec![0x22, 0x33]);
    }
}
