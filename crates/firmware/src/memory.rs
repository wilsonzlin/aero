pub trait MemoryBus {
    fn read_u8(&mut self, addr: u64) -> u8;
    fn write_u8(&mut self, addr: u64, value: u8);

    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (i, out) in buf.iter_mut().enumerate() {
            *out = self.read_u8(paddr + i as u64);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, &b) in buf.iter().enumerate() {
            self.write_u8(paddr + i as u64, b);
        }
    }

    fn read_u16(&mut self, addr: u64) -> u16 {
        let lo = self.read_u8(addr) as u16;
        let hi = self.read_u8(addr + 1) as u16;
        lo | (hi << 8)
    }

    fn write_u16(&mut self, addr: u64, value: u16) {
        self.write_u8(addr, (value & 0xFF) as u8);
        self.write_u8(addr + 1, (value >> 8) as u8);
    }

    fn read_u32(&mut self, addr: u64) -> u32 {
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

    fn read_bytes(&mut self, addr: u64, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.read_u8(addr + i as u64);
        }
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        for (i, b) in bytes.iter().copied().enumerate() {
            self.write_u8(addr + i as u64, b);
        }
    }
}

/// Convert a real-mode segment:offset pair into a physical address.
#[inline]
pub fn real_addr(seg: u16, off: u16) -> u64 {
    ((seg as u64) << 4) + (off as u64)
}

/// Encode a far pointer as a 32-bit value (offset in low word, segment in high word).
#[inline]
pub fn make_far_ptr(seg: u16, off: u16) -> u32 {
    ((seg as u32) << 16) | (off as u32)
}

/// Decode a far pointer and convert it to a physical address.
#[inline]
pub fn far_ptr_to_phys(ptr: u32) -> u64 {
    let off = (ptr & 0xFFFF) as u16;
    let seg = (ptr >> 16) as u16;
    real_addr(seg, off)
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
    fn read_u8(&mut self, addr: u64) -> u8 {
        self.data[addr as usize]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.data[addr as usize] = value;
    }

    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            buf.fill(0xFF);
            return;
        };
        let Some(end) = start.checked_add(buf.len()) else {
            buf.fill(0xFF);
            return;
        };
        if start >= self.data.len() {
            buf.fill(0xFF);
            return;
        }
        let end_clamped = end.min(self.data.len());
        let copied = end_clamped - start;
        buf[..copied].copy_from_slice(&self.data[start..end_clamped]);
        if copied < buf.len() {
            buf[copied..].fill(0xFF);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        let Some(end) = start.checked_add(buf.len()) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let end_clamped = end.min(self.data.len());
        let copied = end_clamped - start;
        self.data[start..end_clamped].copy_from_slice(&buf[..copied]);
    }

    fn read_bytes(&mut self, addr: u64, out: &mut [u8]) {
        self.read_physical(addr, out);
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        self.write_physical(addr, bytes);
    }
}

// Allow firmware helpers to operate directly on the canonical guest memory bus.
impl<T: memory::MemoryBus + ?Sized> MemoryBus for T {
    fn read_u8(&mut self, addr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, addr)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, addr, value);
    }

    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        memory::MemoryBus::read_physical(self, paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        memory::MemoryBus::write_physical(self, paddr, buf);
    }

    fn read_bytes(&mut self, addr: u64, out: &mut [u8]) {
        // Treat `read_bytes` as an alias for physical reads so firmware call sites can efficiently
        // transfer larger buffers (e.g. VBE palette tables) without byte-at-a-time loops.
        memory::MemoryBus::read_physical(self, addr, out);
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        memory::MemoryBus::write_physical(self, addr, bytes);
    }
}
