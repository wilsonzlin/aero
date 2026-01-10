use std::io;

pub trait DiskBackend: Send {
    /// Total disk size in bytes.
    fn len(&self) -> u64;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

pub struct VirtualDrive {
    sector_size: u32,
    backend: Box<dyn DiskBackend>,
}

impl VirtualDrive {
    pub fn new(sector_size: u32, backend: Box<dyn DiskBackend>) -> Self {
        Self {
            sector_size,
            backend,
        }
    }

    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.backend.len()
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.backend.len() / u64::from(self.sector_size)
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.backend.read_at(offset, buf)
    }

    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.backend.write_at(offset, buf)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.backend.flush()
    }
}

impl std::fmt::Debug for VirtualDrive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualDrive")
            .field("sector_size", &self.sector_size)
            .field("capacity_bytes", &self.capacity_bytes())
            .finish()
    }
}

