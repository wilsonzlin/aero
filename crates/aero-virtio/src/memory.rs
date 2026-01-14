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
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy bytes from guest memory into `dst`.
    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError>;

    /// Copy bytes from `src` into guest memory.
    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError>;
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

    /// Borrow a slice of guest RAM.
    ///
    /// This helper is intentionally implemented on [`GuestRam`] directly (rather than
    /// [`GuestMemory`]) so callers can inspect test memory without requiring the virtio memory
    /// abstraction to expose borrowed slices (which is not sound for shared-memory WASM guests).
    pub fn get_slice(&self, addr: u64, len: usize) -> Result<&[u8], GuestMemoryError> {
        check_range(self.len(), addr, len)?;
        let start = addr as usize;
        let end = start + len;
        Ok(&self.data[start..end])
    }

    /// Borrow a mutable slice of guest RAM.
    ///
    /// See [`get_slice`] for why this is an inherent method on [`GuestRam`].
    pub fn get_slice_mut(&mut self, addr: u64, len: usize) -> Result<&mut [u8], GuestMemoryError> {
        check_range(self.len(), addr, len)?;
        let start = addr as usize;
        let end = start + len;
        Ok(&mut self.data[start..end])
    }
}

impl GuestMemory for GuestRam {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        check_range(self.len(), addr, dst.len())?;
        if dst.is_empty() {
            return Ok(());
        }
        let start = addr as usize;
        let end = start + dst.len();
        dst.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        check_range(self.len(), addr, src.len())?;
        if src.is_empty() {
            return Ok(());
        }
        let start = addr as usize;
        let end = start + src.len();
        self.data[start..end].copy_from_slice(src);
        Ok(())
    }
}

fn check_range(size: u64, addr: u64, len: usize) -> Result<(), GuestMemoryError> {
    let end = addr
        .checked_add(len as u64)
        .ok_or(GuestMemoryError::OutOfBounds { addr, len })?;
    if end > size {
        return Err(GuestMemoryError::OutOfBounds { addr, len });
    }
    Ok(())
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
    let mut buf = [0u8; 1];
    mem.read(addr, &mut buf)?;
    Ok(buf[0])
}

pub fn write_u8<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u8,
) -> Result<(), GuestMemoryError> {
    mem.write(addr, &[value])
}

pub fn read_u16_le<M: GuestMemory + ?Sized>(mem: &M, addr: u64) -> Result<u16, GuestMemoryError> {
    let mut buf = [0u8; 2];
    mem.read(addr, &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

pub fn write_u16_le<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u16,
) -> Result<(), GuestMemoryError> {
    let bytes = value.to_le_bytes();
    mem.write(addr, &bytes)
}

pub fn read_u32_le<M: GuestMemory + ?Sized>(mem: &M, addr: u64) -> Result<u32, GuestMemoryError> {
    let mut buf = [0u8; 4];
    mem.read(addr, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

pub fn write_u32_le<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u32,
) -> Result<(), GuestMemoryError> {
    let bytes = value.to_le_bytes();
    mem.write(addr, &bytes)
}

pub fn read_u64_le<M: GuestMemory + ?Sized>(mem: &M, addr: u64) -> Result<u64, GuestMemoryError> {
    let mut buf = [0u8; 8];
    mem.read(addr, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

pub fn write_u64_le<M: GuestMemory + ?Sized>(
    mem: &mut M,
    addr: u64,
    value: u64,
) -> Result<(), GuestMemoryError> {
    let bytes = value.to_le_bytes();
    mem.write(addr, &bytes)
}
