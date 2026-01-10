//! Guest physical memory abstraction.
//!
//! The GPU device model needs to read/write guest RAM for command rings and
//! upload/download operations. The real emulator will provide an implementation
//! backed by its MMU; tests use a simple `Vec<u8>` implementation.

use core::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestMemoryError {
    pub addr: u64,
    pub len: usize,
}

impl fmt::Display for GuestMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "guest memory access out of bounds: addr=0x{:x}, len=0x{:x}",
            self.addr, self.len
        )
    }
}

impl std::error::Error for GuestMemoryError {}

pub trait GuestMemory {
    fn len(&self) -> u64;
    fn read(&self, paddr: u64, buf: &mut [u8]) -> Result<(), GuestMemoryError>;
    fn write(&mut self, paddr: u64, data: &[u8]) -> Result<(), GuestMemoryError>;

    fn read_u32_le(&self, paddr: u64) -> Result<u32, GuestMemoryError> {
        let mut bytes = [0u8; 4];
        self.read(paddr, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_u32_le(&mut self, paddr: u64, value: u32) -> Result<(), GuestMemoryError> {
        self.write(paddr, &value.to_le_bytes())
    }

    fn read_u64_le(&self, paddr: u64) -> Result<u64, GuestMemoryError> {
        let mut bytes = [0u8; 8];
        self.read(paddr, &mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn write_u64_le(&mut self, paddr: u64, value: u64) -> Result<(), GuestMemoryError> {
        self.write(paddr, &value.to_le_bytes())
    }

    fn write_zeros(&mut self, paddr: u64, len: usize) -> Result<(), GuestMemoryError> {
        const CHUNK: usize = 1024;
        let scratch = [0u8; CHUNK];
        let mut offset = 0usize;
        while offset < len {
            let n = (len - offset).min(CHUNK);
            self.write(paddr + offset as u64, &scratch[..n])?;
            offset += n;
        }
        Ok(())
    }
}

/// Convenience helpers implemented for all `GuestMemory`, including trait objects.
pub trait GuestMemoryExt: GuestMemory {
    fn read_exact<const N: usize>(&self, paddr: u64) -> Result<[u8; N], GuestMemoryError> {
        let mut out = [0u8; N];
        self.read(paddr, &mut out)?;
        Ok(out)
    }
}

impl<T: GuestMemory + ?Sized> GuestMemoryExt for T {}

/// Simple guest memory implementation backed by a contiguous `Vec<u8>`.
#[derive(Clone, Debug)]
pub struct VecGuestMemory {
    mem: Vec<u8>,
}

impl VecGuestMemory {
    pub fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.mem
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.mem
    }
}

impl GuestMemory for VecGuestMemory {
    fn len(&self) -> u64 {
        self.mem.len() as u64
    }

    fn read(&self, paddr: u64, buf: &mut [u8]) -> Result<(), GuestMemoryError> {
        let start = paddr as usize;
        let end = start.checked_add(buf.len()).ok_or(GuestMemoryError {
            addr: paddr,
            len: buf.len(),
        })?;
        let slice = self.mem.get(start..end).ok_or(GuestMemoryError {
            addr: paddr,
            len: buf.len(),
        })?;
        buf.copy_from_slice(slice);
        Ok(())
    }

    fn write(&mut self, paddr: u64, data: &[u8]) -> Result<(), GuestMemoryError> {
        let start = paddr as usize;
        let end = start.checked_add(data.len()).ok_or(GuestMemoryError {
            addr: paddr,
            len: data.len(),
        })?;
        let slice = self.mem.get_mut(start..end).ok_or(GuestMemoryError {
            addr: paddr,
            len: data.len(),
        })?;
        slice.copy_from_slice(data);
        Ok(())
    }
}
