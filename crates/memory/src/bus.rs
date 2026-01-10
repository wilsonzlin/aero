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
                    return match &mut region.kind {
                        RegionKind::Mmio(handler) => handler.read(offset, size) & mask_for_size(size),
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
