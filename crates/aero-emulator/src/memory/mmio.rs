use std::fmt;

use crate::memory::MemoryBus;

pub trait MmioDevice: fmt::Debug {
    fn read(&self, offset: u64, data: &mut [u8]);
    fn write(&mut self, offset: u64, data: &[u8]);
}

#[derive(Debug)]
pub struct MmioRegion {
    pub base: u64,
    pub len: u64,
    pub dev: Box<dyn MmioDevice>,
}

impl MmioRegion {
    pub fn new(base: u64, len: u64, dev: Box<dyn MmioDevice>) -> Self {
        Self { base, len, dev }
    }

    fn contains(&self, addr: u64) -> bool {
        self.base <= addr && addr < self.base + self.len
    }
}

/// Simple RAM + MMIO router for unit tests.
///
/// Unmapped reads return `0` and unmapped writes are ignored to keep behavior
/// deterministic (matching `LinearMemory`).
#[derive(Debug)]
pub struct MmioMemory {
    ram: Vec<u8>,
    regions: Vec<MmioRegion>,
}

impl MmioMemory {
    pub fn new(ram_size: usize) -> Self {
        Self {
            ram: vec![0u8; ram_size],
            regions: Vec::new(),
        }
    }

    pub fn map_mmio(&mut self, base: u64, len: u64, dev: Box<dyn MmioDevice>) {
        if len == 0 {
            return;
        }
        self.regions.push(MmioRegion::new(base, len, dev));
    }

    fn find_region_index(&self, addr: u64) -> Option<usize> {
        // Last mapping wins to keep behavior deterministic even with overlaps.
        self.regions.iter().rposition(|r| r.contains(addr))
    }
}

impl MemoryBus for MmioMemory {
    fn read_u8(&self, addr: u64) -> u8 {
        if let Some(idx) = self.find_region_index(addr) {
            let region = &self.regions[idx];
            let mut tmp = [0u8; 1];
            region.dev.read(addr - region.base, &mut tmp);
            return tmp[0];
        }

        self.ram.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        if let Some(idx) = self.find_region_index(addr) {
            let region = &mut self.regions[idx];
            region.dev.write(addr - region.base, &[value]);
            return;
        }

        if let Some(slot) = self.ram.get_mut(addr as usize) {
            *slot = value;
        }
    }

    fn read_physical(&self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        if let Some(idx) = self.find_region_index(paddr) {
            let region = &self.regions[idx];
            if paddr + buf.len() as u64 <= region.base + region.len {
                region.dev.read(paddr - region.base, buf);
                return;
            }
        }

        let start = paddr as usize;
        if let Some(slice) = self.ram.get(start..start + buf.len()) {
            buf.copy_from_slice(slice);
            return;
        }

        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = self.read_u8(paddr + i as u64);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        if let Some(idx) = self.find_region_index(paddr) {
            let region = &mut self.regions[idx];
            if paddr + buf.len() as u64 <= region.base + region.len {
                region.dev.write(paddr - region.base, buf);
                return;
            }
        }

        let start = paddr as usize;
        if let Some(slice) = self.ram.get_mut(start..start + buf.len()) {
            slice.copy_from_slice(buf);
            return;
        }

        for (i, b) in buf.iter().copied().enumerate() {
            self.write_u8(paddr + i as u64, b);
        }
    }
}
