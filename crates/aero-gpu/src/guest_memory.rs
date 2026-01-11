//! Guest physical memory abstraction used by the host-side AeroGPU executor.
//!
//! The real emulator will provide an implementation backed by its memory system.
//! For now we keep the trait intentionally small so it can be implemented from
//! both native Rust and WASM (via future JS glue).

use core::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestMemoryError {
    pub gpa: u64,
    pub len: usize,
}

impl fmt::Display for GuestMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "guest memory access out of bounds: gpa=0x{:x}, len=0x{:x}",
            self.gpa, self.len
        )
    }
}

impl std::error::Error for GuestMemoryError {}

/// Minimal guest memory read interface.
pub trait GuestMemory {
    fn read(&self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError>;
}

/// Simple contiguous in-memory guest RAM implementation for tests.
#[derive(Clone, Debug)]
pub struct VecGuestMemory {
    mem: Vec<u8>,
}

impl VecGuestMemory {
    pub fn new(size_bytes: usize) -> Self {
        Self {
            mem: vec![0u8; size_bytes],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.mem
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.mem
    }

    pub fn write(&mut self, gpa: u64, data: &[u8]) -> Result<(), GuestMemoryError> {
        let start = usize::try_from(gpa).map_err(|_| GuestMemoryError {
            gpa,
            len: data.len(),
        })?;
        let end = start.checked_add(data.len()).ok_or(GuestMemoryError {
            gpa,
            len: data.len(),
        })?;
        let slice = self.mem.get_mut(start..end).ok_or(GuestMemoryError {
            gpa,
            len: data.len(),
        })?;
        slice.copy_from_slice(data);
        Ok(())
    }
}

impl GuestMemory for VecGuestMemory {
    fn read(&self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        let start = usize::try_from(gpa).map_err(|_| GuestMemoryError {
            gpa,
            len: dst.len(),
        })?;
        let end = start.checked_add(dst.len()).ok_or(GuestMemoryError { gpa, len: dst.len() })?;
        let slice = self.mem.get(start..end).ok_or(GuestMemoryError { gpa, len: dst.len() })?;
        dst.copy_from_slice(slice);
        Ok(())
    }
}

