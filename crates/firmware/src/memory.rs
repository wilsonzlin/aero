pub trait MemoryBus {
    fn read_u8(&self, addr: u64) -> u8;
    fn write_u8(&mut self, addr: u64, value: u8);

    fn read_physical(&self, paddr: u64, buf: &mut [u8]) {
        for (i, out) in buf.iter_mut().enumerate() {
            *out = self.read_u8(paddr + i as u64);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, &b) in buf.iter().enumerate() {
            self.write_u8(paddr + i as u64, b);
        }
    }

    fn read_u16(&self, addr: u64) -> u16 {
        let lo = self.read_u8(addr) as u16;
        let hi = self.read_u8(addr + 1) as u16;
        lo | (hi << 8)
    }

    fn write_u16(&mut self, addr: u64, value: u16) {
        self.write_u8(addr, (value & 0xFF) as u8);
        self.write_u8(addr + 1, (value >> 8) as u8);
    }

    fn read_u32(&self, addr: u64) -> u32 {
        let b0 = self.read_u8(addr) as u32;
        let b1 = self.read_u8(addr + 1) as u32;
        let b2 = self.read_u8(addr + 2) as u32;
        let b3 = self.read_u8(addr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        self.write_u8(addr, (value & 0xFF) as u8);
        self.write_u8(addr + 1, ((value >> 8) & 0xFF) as u8);
        self.write_u8(addr + 2, ((value >> 16) & 0xFF) as u8);
        self.write_u8(addr + 3, ((value >> 24) & 0xFF) as u8);
    }
}

#[derive(Debug, Clone)]
pub struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }
}

impl MemoryBus for VecMemory {
    fn read_u8(&self, addr: u64) -> u8 {
        self.data[addr as usize]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.data[addr as usize] = value;
    }
}

