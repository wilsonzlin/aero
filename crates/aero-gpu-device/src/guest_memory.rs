use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestMemoryError {
    AddressOverflow,
    OutOfBounds {
        guest_phys_addr: u64,
        len: usize,
        backing_size: usize,
    },
}

impl fmt::Display for GuestMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuestMemoryError::AddressOverflow => write!(f, "guest memory address overflow"),
            GuestMemoryError::OutOfBounds {
                guest_phys_addr,
                len,
                backing_size,
            } => write!(
                f,
                "guest memory read/write out of bounds (addr=0x{guest_phys_addr:x}, len=0x{len:x}, backing_size=0x{backing_size:x})"
            ),
        }
    }
}

impl std::error::Error for GuestMemoryError {}

pub trait GuestMemory {
    fn read(&self, guest_phys_addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError>;
}

#[derive(Debug, Clone)]
pub struct VecGuestMemory {
    bytes: Vec<u8>,
}

impl VecGuestMemory {
    pub fn new(size: usize) -> Self {
        Self {
            bytes: vec![0u8; size],
        }
    }

    pub fn write(&mut self, guest_phys_addr: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        let len = src.len();
        let end = guest_phys_addr
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::AddressOverflow)?;
        let start = usize::try_from(guest_phys_addr).map_err(|_| GuestMemoryError::AddressOverflow)?;
        let end = usize::try_from(end).map_err(|_| GuestMemoryError::AddressOverflow)?;
        if end > self.bytes.len() {
            return Err(GuestMemoryError::OutOfBounds {
                guest_phys_addr,
                len,
                backing_size: self.bytes.len(),
            });
        }
        self.bytes[start..end].copy_from_slice(src);
        Ok(())
    }
}

impl GuestMemory for VecGuestMemory {
    fn read(&self, guest_phys_addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        let len = dst.len();
        let end = guest_phys_addr
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::AddressOverflow)?;
        let start = usize::try_from(guest_phys_addr).map_err(|_| GuestMemoryError::AddressOverflow)?;
        let end = usize::try_from(end).map_err(|_| GuestMemoryError::AddressOverflow)?;
        if end > self.bytes.len() {
            return Err(GuestMemoryError::OutOfBounds {
                guest_phys_addr,
                len,
                backing_size: self.bytes.len(),
            });
        }
        dst.copy_from_slice(&self.bytes[start..end]);
        Ok(())
    }
}
