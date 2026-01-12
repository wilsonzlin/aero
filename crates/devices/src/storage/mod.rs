use std::io;

use aero_storage_adapters::AeroVirtualDiskAsDeviceBackend;

pub trait DiskBackend: Send {
    /// Total disk size in bytes.
    fn len(&self) -> u64;
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

impl DiskBackend for AeroVirtualDiskAsDeviceBackend {
    fn len(&self) -> u64 {
        self.capacity_bytes()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.read_at_aligned(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.write_at_aligned(offset, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        AeroVirtualDiskAsDeviceBackend::flush(self)
    }
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

    /// Wrap a boxed [`aero_storage::VirtualDisk`] as a `aero-devices` [`DiskBackend`].
    ///
    /// This is a convenience for the common case where the disk is already stored behind a
    /// trait object (`Box<dyn VirtualDisk>`). The adapter enforces 512-byte alignment and
    /// bounds checks at the device boundary.
    pub fn new_from_aero_virtual_disk(
        disk: Box<dyn aero_storage::VirtualDisk + Send>,
    ) -> Self {
        Self::new(
            512,
            Box::new(AeroVirtualDiskAsDeviceBackend::new(disk)),
        )
    }

    pub fn new_from_aero_storage<D>(disk: D) -> Self
    where
        D: aero_storage::VirtualDisk + Send + 'static,
    {
        Self::new_from_aero_virtual_disk(Box::new(disk))
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
