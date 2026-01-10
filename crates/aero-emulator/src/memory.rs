pub trait MemoryBus {
    fn read_u8(&self, addr: u64) -> u8;
    fn write_u8(&mut self, addr: u64, value: u8);

    fn read_u16(&self, addr: u64) -> u16 {
        u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr + 1)])
    }

    fn write_u16(&mut self, addr: u64, value: u16) {
        let bytes = value.to_le_bytes();
        self.write_u8(addr, bytes[0]);
        self.write_u8(addr + 1, bytes[1]);
    }

    fn read_u32(&self, addr: u64) -> u32 {
        u32::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr + 1),
            self.read_u8(addr + 2),
            self.read_u8(addr + 3),
        ])
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        let bytes = value.to_le_bytes();
        for (i, b) in bytes.into_iter().enumerate() {
            self.write_u8(addr + i as u64, b);
        }
    }

    fn read_u64(&self, addr: u64) -> u64 {
        u64::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr + 1),
            self.read_u8(addr + 2),
            self.read_u8(addr + 3),
            self.read_u8(addr + 4),
            self.read_u8(addr + 5),
            self.read_u8(addr + 6),
            self.read_u8(addr + 7),
        ])
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        let bytes = value.to_le_bytes();
        for (i, b) in bytes.into_iter().enumerate() {
            self.write_u8(addr + i as u64, b);
        }
    }

    fn read_physical(&self, paddr: u64, buf: &mut [u8]) {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = self.read_u8(paddr + i as u64);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, b) in buf.iter().copied().enumerate() {
            self.write_u8(paddr + i as u64, b);
        }
    }
}

#[derive(Clone)]
pub struct LinearMemory {
    data: Vec<u8>,
}

impl LinearMemory {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

impl MemoryBus for LinearMemory {
    fn read_u8(&self, addr: u64) -> u8 {
        self.data.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        if let Some(slot) = self.data.get_mut(addr as usize) {
            *slot = value;
        }
    }
}
