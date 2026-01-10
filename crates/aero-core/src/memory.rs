#![forbid(unsafe_code)]

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    OutOfBounds { addr: u64, len: usize },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::OutOfBounds { addr, len } => {
                write!(f, "memory access out of bounds: addr={addr:#x} len={len}")
            }
        }
    }
}

impl std::error::Error for MemoryError {}

pub trait Memory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemoryError>;
    fn write(&mut self, addr: u64, buf: &[u8]) -> Result<(), MemoryError>;

    fn read_u8(&self, addr: u64) -> Result<u8, MemoryError> {
        let mut b = [0u8; 1];
        self.read(addr, &mut b)?;
        Ok(b[0])
    }

    fn read_u16(&self, addr: u64) -> Result<u16, MemoryError> {
        let mut b = [0u8; 2];
        self.read(addr, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    fn read_u32(&self, addr: u64) -> Result<u32, MemoryError> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn read_u64(&self, addr: u64) -> Result<u64, MemoryError> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn write_u8(&mut self, addr: u64, val: u8) -> Result<(), MemoryError> {
        self.write(addr, &[val])
    }

    fn write_u16(&mut self, addr: u64, val: u16) -> Result<(), MemoryError> {
        self.write(addr, &val.to_le_bytes())
    }

    fn write_u32(&mut self, addr: u64, val: u32) -> Result<(), MemoryError> {
        self.write(addr, &val.to_le_bytes())
    }

    fn write_u64(&mut self, addr: u64, val: u64) -> Result<(), MemoryError> {
        self.write(addr, &val.to_le_bytes())
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

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Memory for VecMemory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemoryError> {
        let addr = addr as usize;
        let end = addr
            .checked_add(buf.len())
            .ok_or(MemoryError::OutOfBounds {
                addr: addr as u64,
                len: buf.len(),
            })?;
        let src = self.data.get(addr..end).ok_or(MemoryError::OutOfBounds {
            addr: addr as u64,
            len: buf.len(),
        })?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write(&mut self, addr: u64, buf: &[u8]) -> Result<(), MemoryError> {
        let addr = addr as usize;
        let end = addr
            .checked_add(buf.len())
            .ok_or(MemoryError::OutOfBounds {
                addr: addr as u64,
                len: buf.len(),
            })?;
        let dst = self
            .data
            .get_mut(addr..end)
            .ok_or(MemoryError::OutOfBounds {
                addr: addr as u64,
                len: buf.len(),
            })?;
        dst.copy_from_slice(buf);
        Ok(())
    }
}
