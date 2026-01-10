use crate::util::checked_range;
use crate::{DiskError, Result, StorageBackend};

pub const SECTOR_SIZE: usize = 512;

/// A fixed-capacity virtual disk.
///
/// Implementations are byte-addressed (`read_at` / `write_at`) for easy composition with
/// block caches and sparse formats, but the emulator-facing API is *sector-based* via
/// `read_sectors` / `write_sectors`.
pub trait VirtualDisk {
    /// Disk capacity in bytes.
    fn capacity_bytes(&self) -> u64;

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<()> {
        if buf.len() % SECTOR_SIZE != 0 {
            return Err(DiskError::UnalignedLength {
                len: buf.len(),
                alignment: SECTOR_SIZE,
            });
        }
        let offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        self.read_at(offset, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<()> {
        if buf.len() % SECTOR_SIZE != 0 {
            return Err(DiskError::UnalignedLength {
                len: buf.len(),
                alignment: SECTOR_SIZE,
            });
        }
        let offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        self.write_at(offset, buf)
    }
}

/// A raw disk image stored in a byte backend (OPFS file, ArrayBuffer, etc.).
pub struct RawDisk<B> {
    backend: B,
    capacity: u64,
}

impl<B: StorageBackend> RawDisk<B> {
    pub fn create(mut backend: B, capacity_bytes: u64) -> Result<Self> {
        backend.set_len(capacity_bytes)?;
        Ok(Self {
            backend,
            capacity: capacity_bytes,
        })
    }

    pub fn open(mut backend: B) -> Result<Self> {
        let capacity = backend.len()?;
        Ok(Self { backend, capacity })
    }

    pub fn into_backend(self) -> B {
        self.backend
    }
}

impl<B: StorageBackend> VirtualDisk for RawDisk<B> {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity)?;
        self.backend.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity)?;
        self.backend.write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<()> {
        self.backend.flush()
    }
}
