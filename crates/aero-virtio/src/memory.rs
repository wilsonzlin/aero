use core::ops::{Deref, DerefMut};

/// Errors produced by [`GuestMemory`] implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemoryError {
    /// The requested address range was outside the guest memory.
    OutOfBounds { addr: u64, len: usize },
}

/// A very small abstraction for accessing guest physical memory.
///
/// The wider emulator will likely provide a richer memory bus, but virtio
/// devices need a way to read/write the descriptor rings and the referenced
/// buffers. This trait keeps the virtio logic decoupled from the rest of the
/// emulator and is easy to mock in tests.
pub trait GuestMemory {
    /// Total length of the guest memory address space handled by this object.
    fn len(&self) -> u64;

    /// Copy bytes from guest memory into `dst`.
    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError>;

    /// Copy bytes from `src` into guest memory.
    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError>;

    /// Borrow a slice of guest memory.
    fn get_slice(&self, addr: u64, len: usize) -> Result<&[u8], GuestMemoryError>;

    /// Borrow a mutable slice of guest memory.
    fn get_slice_mut(&mut self, addr: u64, len: usize) -> Result<&mut [u8], GuestMemoryError>;
}

/// Contiguous guest RAM backed by a `Vec<u8>`.
#[derive(Clone, Debug)]
pub struct GuestRam {
    data: Vec<u8>,
}

impl GuestRam {
    /// Allocate a new [`GuestRam`] of `size` bytes.
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    /// Borrow the entire backing store.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Borrow the entire backing store mutably.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl GuestMemory for GuestRam {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        dst.copy_from_slice(self.get_slice(addr, dst.len())?);
        Ok(())
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        self.get_slice_mut(addr, src.len())?.copy_from_slice(src);
        Ok(())
    }

    fn get_slice(&self, addr: u64, len: usize) -> Result<&[u8], GuestMemoryError> {
        let end = addr
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfBounds { addr, len })?;
        if end > self.len() {
            return Err(GuestMemoryError::OutOfBounds { addr, len });
        }
        Ok(&self.data[addr as usize..end as usize])
    }

    fn get_slice_mut(&mut self, addr: u64, len: usize) -> Result<&mut [u8], GuestMemoryError> {
        let end = addr
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfBounds { addr, len })?;
        if end > self.len() {
            return Err(GuestMemoryError::OutOfBounds { addr, len });
        }
        Ok(&mut self.data[addr as usize..end as usize])
    }
}

/// A helper newtype for reading little-endian integers from guest memory.
///
/// This is intentionally tiny instead of pulling in `byteorder`.
#[derive(Clone, Copy)]
pub struct Le<T>(pub T);

impl<T> Deref for Le<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Le<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub fn read_u8<M: GuestMemory + ?Sized>(mem: &M, addr: u64) -> Result<u8, GuestMemoryError> {
    Ok(mem.get_slice(addr, 1)?[0])
}

pub fn write_u8<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u8,
) -> Result<(), GuestMemoryError> {
    mem.get_slice_mut(addr, 1)?[0] = value;
    Ok(())
}

pub fn read_u16_le<M: GuestMemory + ?Sized>(mem: &M, addr: u64) -> Result<u16, GuestMemoryError> {
    let bytes = mem.get_slice(addr, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub fn write_u16_le<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u16,
) -> Result<(), GuestMemoryError> {
    let bytes = value.to_le_bytes();
    mem.write(addr, &bytes)?;
    Ok(())
}

pub fn read_u32_le<M: GuestMemory + ?Sized>(mem: &M, addr: u64) -> Result<u32, GuestMemoryError> {
    let bytes = mem.get_slice(addr, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub fn write_u32_le<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u32,
) -> Result<(), GuestMemoryError> {
    let bytes = value.to_le_bytes();
    mem.write(addr, &bytes)?;
    Ok(())
}

pub fn read_u64_le<M: GuestMemory + ?Sized>(mem: &M, addr: u64) -> Result<u64, GuestMemoryError> {
    let bytes = mem.get_slice(addr, 8)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

pub fn write_u64_le<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u64,
) -> Result<(), GuestMemoryError> {
    let bytes = value.to_le_bytes();
    mem.write(addr, &bytes)?;
    Ok(())
}
