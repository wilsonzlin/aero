use crate::exception::Exception;

pub trait CpuBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception>;
    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception>;
    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception>;
    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception>;
    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception>;

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception>;
    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception>;
    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception>;
    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception>;
    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception>;

    /// Fetch up to 15 bytes from instruction memory. Implementations should
    /// allow reads that cross page boundaries (the caller handles page faults
    /// separately), but for tests we just bounds-check.
    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception>;

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception>;
    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception>;
}

/// Identity-mapped memory bus used by unit tests.
#[derive(Debug, Clone)]
pub struct FlatTestBus {
    mem: Vec<u8>,
}

impl FlatTestBus {
    pub fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    pub fn load(&mut self, addr: u64, data: &[u8]) {
        let start = addr as usize;
        let end = start + data.len();
        self.mem[start..end].copy_from_slice(data);
    }

    pub fn slice(&self, addr: u64, len: usize) -> &[u8] {
        let start = addr as usize;
        let end = start + len;
        &self.mem[start..end]
    }
}

impl CpuBus for FlatTestBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.mem
            .get(vaddr as usize)
            .copied()
            .ok_or(Exception::MemoryFault)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let lo = self.read_u8(vaddr)? as u16;
        let hi = self.read_u8(vaddr + 1)? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut v = 0u32;
        for i in 0..4 {
            v |= (self.read_u8(vaddr + i)? as u32) << (i * 8);
        }
        Ok(v)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut v = 0u64;
        for i in 0..8 {
            v |= (self.read_u8(vaddr + i)? as u64) << (i * 8);
        }
        Ok(v)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut v = 0u128;
        for i in 0..16 {
            v |= (self.read_u8(vaddr + i)? as u128) << (i * 8);
        }
        Ok(v)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        let slot = self
            .mem
            .get_mut(vaddr as usize)
            .ok_or(Exception::MemoryFault)?;
        *slot = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_u8(vaddr, (val & 0xFF) as u8)?;
        self.write_u8(vaddr + 1, (val >> 8) as u8)?;
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        for i in 0..4 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        for i in 0..8 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        for i in 0..16 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = self.read_u8(vaddr + i as u64)?;
        }
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}
