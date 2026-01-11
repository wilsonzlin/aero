use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestMemoryError {
    OutOfBounds {
        gpa: u64,
        len: usize,
    },
}

impl fmt::Display for GuestMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuestMemoryError::OutOfBounds { gpa, len } => {
                write!(f, "guest memory read out of bounds (gpa=0x{gpa:x}, len={len})")
            }
        }
    }
}

impl std::error::Error for GuestMemoryError {}

pub trait GuestMemory {
    fn read(&self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError>;
}

/// A simple in-memory guest memory implementation backed by a single `Vec<u8>`.
///
/// The address space starts at GPA 0.
#[derive(Debug, Clone)]
pub struct VecGuestMemory {
    data: Vec<u8>,
}

impl VecGuestMemory {
    pub fn new(size_bytes: usize) -> Self {
        Self {
            data: vec![0u8; size_bytes],
        }
    }

    pub fn write(&mut self, gpa: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        let start: usize = gpa.try_into().map_err(|_| GuestMemoryError::OutOfBounds {
            gpa,
            len: src.len(),
        })?;
        let end = start.checked_add(src.len()).ok_or(GuestMemoryError::OutOfBounds {
            gpa,
            len: src.len(),
        })?;
        let dst = self
            .data
            .get_mut(start..end)
            .ok_or(GuestMemoryError::OutOfBounds { gpa, len: src.len() })?;
        dst.copy_from_slice(src);
        Ok(())
    }
}

impl GuestMemory for VecGuestMemory {
    fn read(&self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        let start: usize = gpa.try_into().map_err(|_| GuestMemoryError::OutOfBounds {
            gpa,
            len: dst.len(),
        })?;
        let end = start.checked_add(dst.len()).ok_or(GuestMemoryError::OutOfBounds {
            gpa,
            len: dst.len(),
        })?;
        let src = self
            .data
            .get(start..end)
            .ok_or(GuestMemoryError::OutOfBounds { gpa, len: dst.len() })?;
        dst.copy_from_slice(src);
        Ok(())
    }
}

