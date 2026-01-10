pub const DEFAULT_LAPIC_BASE: u64 = 0xFEE0_0000;
pub const DEFAULT_IOAPIC_BASE: u64 = 0xFEC0_0000;

#[derive(Debug, Clone)]
pub struct Pic {
    pub master_mask: u8,
    pub slave_mask: u8,
}

impl Pic {
    pub fn new() -> Self {
        Self {
            master_mask: 0xFF,
            slave_mask: 0xFF,
        }
    }

    fn read_u8(&mut self, port: u16) -> u8 {
        match port {
            0x21 => self.master_mask,
            0xA1 => self.slave_mask,
            _ => 0,
        }
    }

    fn write_u8(&mut self, port: u16, val: u8) {
        match port {
            0x21 => self.master_mask = val,
            0xA1 => self.slave_mask = val,
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pit {
    ticks: u64,
    sub_ns: u128,
}

impl Pit {
    pub const HZ: u64 = 1_193_182;

    pub fn new() -> Self {
        Self { ticks: 0, sub_ns: 0 }
    }

    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    pub fn advance_ns(&mut self, ns: u64) {
        // Accumulate in units of (ticks * 1e9) to avoid drift.
        let total = self.sub_ns + (ns as u128) * (Self::HZ as u128);
        let delta = total / 1_000_000_000u128;
        self.sub_ns = total % 1_000_000_000u128;
        self.ticks = self.ticks.saturating_add(delta as u64);
    }

    fn read_u8(&mut self, _port: u16) -> u8 {
        0
    }

    fn write_u8(&mut self, _port: u16, _val: u8) {}
}

#[derive(Debug, Clone)]
pub struct Rtc {
    index: u8,
    regs: [u8; 128],
}

impl Rtc {
    pub fn new() -> Self {
        // Deterministic baseline. Guests can set their own time; the emulator
        // must not depend on host wall-clock in tests.
        let mut regs = [0u8; 128];
        regs[0x0B] = 0x02; // 24-hour mode, BCD.
        Self { index: 0, regs }
    }

    fn read_u8(&mut self, port: u16) -> u8 {
        match port {
            0x70 => self.index,
            0x71 => self.regs[(self.index & 0x7F) as usize],
            _ => 0,
        }
    }

    fn write_u8(&mut self, port: u16, val: u8) {
        match port {
            0x70 => self.index = val,
            0x71 => {
                let idx = (self.index & 0x7F) as usize;
                self.regs[idx] = val;
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hpet {
    pub base: u64,
    counter: u64,
    sub_ns: u128,
}

impl Hpet {
    pub const HZ: u64 = 10_000_000;
    pub const MMIO_SIZE: u64 = 0x400;

    pub fn new(base: u64) -> Self {
        Self {
            base,
            counter: 0,
            sub_ns: 0,
        }
    }

    pub fn counter(&self) -> u64 {
        self.counter
    }

    pub fn advance_ns(&mut self, ns: u64) {
        let total = self.sub_ns + (ns as u128) * (Self::HZ as u128);
        let delta = total / 1_000_000_000u128;
        self.sub_ns = total % 1_000_000_000u128;
        self.counter = self.counter.saturating_add(delta as u64);
    }

    fn read_u8(&mut self, offset: u64) -> u8 {
        // Main counter is at 0xF0.
        if (0xF0..0xF0 + 8).contains(&offset) {
            let shift = (offset - 0xF0) * 8;
            return (self.counter >> shift) as u8;
        }
        0
    }

    fn write_u8(&mut self, _offset: u64, _val: u8) {}
}

#[derive(Debug, Clone)]
pub struct MmioPage {
    base: u64,
    data: Vec<u8>,
}

impl MmioPage {
    pub fn new(base: u64, size: usize) -> Self {
        Self {
            base,
            data: vec![0; size],
        }
    }

    fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.base + (self.data.len() as u64)
    }

    fn read_u8(&mut self, addr: u64) -> u8 {
        if !self.contains(addr) {
            return 0;
        }
        self.data[(addr - self.base) as usize]
    }

    fn write_u8(&mut self, addr: u64, val: u8) {
        if !self.contains(addr) {
            return;
        }
        self.data[(addr - self.base) as usize] = val;
    }
}

#[derive(Debug, Clone)]
pub struct PciBus {
    cfg_addr: u32,
}

impl PciBus {
    pub fn new() -> Self {
        Self { cfg_addr: 0 }
    }

    fn read_u8(&mut self, port: u16) -> u8 {
        match port {
            0xCF8..=0xCFB => {
                let shift = (port - 0xCF8) * 8;
                (self.cfg_addr >> shift) as u8
            }
            0xCFC..=0xCFF => {
                // No devices yet. Return all-ones to emulate unmapped config space.
                let _shift = (port - 0xCFC) * 8;
                0xFF
            }
            _ => 0xFF,
        }
    }

    fn write_u8(&mut self, port: u16, val: u8) {
        match port {
            0xCF8..=0xCFB => {
                let shift = (port - 0xCF8) * 8;
                let mask = !(0xFFu32 << shift);
                self.cfg_addr = (self.cfg_addr & mask) | ((val as u32) << shift);
            }
            0xCFC..=0xCFF => {
                // Ignore writes until a PCI model exists.
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct Devices {
    pub pic: Pic,
    pub pit: Pit,
    pub rtc: Rtc,
    pub hpet: Hpet,
    pub lapic: MmioPage,
    pub ioapic: MmioPage,
    pub pci: PciBus,
}

impl Devices {
    pub fn new(hpet_base: u64) -> Self {
        Self {
            pic: Pic::new(),
            pit: Pit::new(),
            rtc: Rtc::new(),
            hpet: Hpet::new(hpet_base),
            lapic: MmioPage::new(DEFAULT_LAPIC_BASE, 0x1000),
            ioapic: MmioPage::new(DEFAULT_IOAPIC_BASE, 0x1000),
            pci: PciBus::new(),
        }
    }

    pub fn io_read_u8(&mut self, port: u16) -> u8 {
        match port {
            0x20 | 0x21 | 0xA0 | 0xA1 => self.pic.read_u8(port),
            0x40..=0x43 => self.pit.read_u8(port),
            0x70 | 0x71 => self.rtc.read_u8(port),
            0xCF8..=0xCFF => self.pci.read_u8(port),
            _ => 0xFF,
        }
    }

    pub fn io_write_u8(&mut self, port: u16, val: u8) {
        match port {
            0x20 | 0x21 | 0xA0 | 0xA1 => self.pic.write_u8(port, val),
            0x40..=0x43 => self.pit.write_u8(port, val),
            0x70 | 0x71 => self.rtc.write_u8(port, val),
            0xCF8..=0xCFF => self.pci.write_u8(port, val),
            _ => {}
        }
    }

    pub fn mmio_read_u8(&mut self, addr: u64) -> Option<u8> {
        if addr >= self.hpet.base && addr < self.hpet.base + Hpet::MMIO_SIZE {
            return Some(self.hpet.read_u8(addr - self.hpet.base));
        }
        if self.lapic.contains(addr) {
            return Some(self.lapic.read_u8(addr));
        }
        if self.ioapic.contains(addr) {
            return Some(self.ioapic.read_u8(addr));
        }
        None
    }

    pub fn mmio_write_u8(&mut self, addr: u64, val: u8) -> bool {
        if addr >= self.hpet.base && addr < self.hpet.base + Hpet::MMIO_SIZE {
            self.hpet.write_u8(addr - self.hpet.base, val);
            return true;
        }
        if self.lapic.contains(addr) {
            self.lapic.write_u8(addr, val);
            return true;
        }
        if self.ioapic.contains(addr) {
            self.ioapic.write_u8(addr, val);
            return true;
        }
        false
    }
}

