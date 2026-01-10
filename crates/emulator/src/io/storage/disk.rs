pub use crate::io::storage::error::{DiskError, DiskResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskFormat {
    Raw,
    Sparse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCachePolicy {
    /// Each write is forwarded to the underlying backend before returning.
    WriteThrough,
    /// Writes may be buffered in memory until `flush()`/eviction.
    WriteBack,
}

/// A byte-addressable random-access storage primitive.
///
/// This is intentionally minimal so it can be implemented on top of browser APIs
/// (e.g. OPFS SyncAccessHandle) without pulling in OS-specific file APIs.
pub trait ByteStorage {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()>;
    fn flush(&mut self) -> DiskResult<()>;
    fn len(&mut self) -> DiskResult<u64>;
    fn set_len(&mut self, len: u64) -> DiskResult<()>;
}

/// Synchronous virtual block device interface.
///
/// Callers are expected to use whole sectors; implementations must return
/// `DiskError::UnalignedBuffer` when given a buffer length that is not a multiple
/// of the sector size.
pub trait DiskBackend {
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;

    /// Alias for `total_sectors()`.
    fn capacity_sectors(&self) -> u64 {
        self.total_sectors()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()>;
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()>;
    fn flush(&mut self) -> DiskResult<()>;

    /// Optional scatter-gather read variant.
    ///
    /// The default implementation forwards each buffer to `read_sectors`.
    fn readv_sectors(&mut self, mut lba: u64, bufs: &mut [&mut [u8]]) -> DiskResult<()> {
        let sector_size = self.sector_size();
        for buf in bufs {
            if buf.len() % sector_size as usize != 0 {
                return Err(DiskError::UnalignedBuffer {
                    len: buf.len(),
                    sector_size,
                });
            }
            let sectors = (buf.len() / sector_size as usize) as u64;
            self.read_sectors(lba, buf)?;
            lba = lba.saturating_add(sectors);
        }
        Ok(())
    }

    /// Optional scatter-gather write variant.
    ///
    /// The default implementation forwards each buffer to `write_sectors`.
    fn writev_sectors(&mut self, mut lba: u64, bufs: &[&[u8]]) -> DiskResult<()> {
        let sector_size = self.sector_size();
        for buf in bufs {
            if buf.len() % sector_size as usize != 0 {
                return Err(DiskError::UnalignedBuffer {
                    len: buf.len(),
                    sector_size,
                });
            }
            let sectors = (buf.len() / sector_size as usize) as u64;
            self.write_sectors(lba, buf)?;
            lba = lba.saturating_add(sectors);
        }
        Ok(())
    }
}

pub struct VirtualDrive {
    backend: Box<dyn DiskBackend>,
    format: DiskFormat,
    sector_size: u32,
    total_sectors: u64,
    write_cache: WriteCachePolicy,
}

impl VirtualDrive {
    pub fn new(
        format: DiskFormat,
        backend: Box<dyn DiskBackend>,
        write_cache: WriteCachePolicy,
    ) -> DiskResult<Self> {
        let sector_size = backend.sector_size();
        if sector_size != 512 && sector_size != 4096 {
            return Err(DiskError::Unsupported("sector size (expected 512 or 4096)"));
        }
        let total_sectors = backend.total_sectors();
        Ok(Self {
            backend,
            format,
            sector_size,
            total_sectors,
            write_cache,
        })
    }

    pub fn format(&self) -> DiskFormat {
        self.format
    }

    pub fn write_cache_policy(&self) -> WriteCachePolicy {
        self.write_cache
    }

    pub fn into_backend(self) -> Box<dyn DiskBackend> {
        self.backend
    }

    fn check_range(&self, lba: u64, bytes: usize) -> DiskResult<u64> {
        if bytes % self.sector_size as usize != 0 {
            return Err(DiskError::UnalignedBuffer {
                len: bytes,
                sector_size: self.sector_size,
            });
        }
        let sectors = (bytes / self.sector_size as usize) as u64;
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors,
        })?;
        if end > self.total_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.total_sectors,
            });
        }
        Ok(sectors)
    }
}

impl DiskBackend for VirtualDrive {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        self.backend.read_sectors(lba, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        self.backend.write_sectors(lba, buf)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.backend.flush()
    }

    fn readv_sectors(&mut self, lba: u64, bufs: &mut [&mut [u8]]) -> DiskResult<()> {
        let bytes: usize = bufs.iter().map(|b| b.len()).sum();
        self.check_range(lba, bytes)?;
        self.backend.readv_sectors(lba, bufs)
    }

    fn writev_sectors(&mut self, lba: u64, bufs: &[&[u8]]) -> DiskResult<()> {
        let bytes: usize = bufs.iter().map(|b| b.len()).sum();
        self.check_range(lba, bytes)?;
        self.backend.writev_sectors(lba, bufs)
    }
}

/// In-memory test backend.
#[derive(Clone, Debug)]
pub struct MemDisk {
    sector_size: u32,
    data: Vec<u8>,
    flushed: bool,
}

impl MemDisk {
    pub fn new(total_sectors: u64) -> Self {
        Self::new_with_sector_size(total_sectors, 512)
    }

    pub fn new_with_sector_size(total_sectors: u64, sector_size: u32) -> Self {
        let len = usize::try_from(total_sectors * sector_size as u64)
            .expect("disk size too large for MemDisk");
        Self {
            sector_size,
            data: vec![0; len],
            flushed: false,
        }
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn was_flushed(&self) -> bool {
        self.flushed
    }

    fn check_range(&self, lba: u64, bytes: usize) -> DiskResult<u64> {
        if bytes % self.sector_size as usize != 0 {
            return Err(DiskError::UnalignedBuffer {
                len: bytes,
                sector_size: self.sector_size,
            });
        }
        let sectors = (bytes / self.sector_size as usize) as u64;
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors(),
        })?;
        if end > self.total_sectors() {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.total_sectors(),
            });
        }
        Ok(sectors)
    }
}

impl DiskBackend for MemDisk {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        (self.data.len() as u64) / self.sector_size as u64
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        let offset = usize::try_from(lba * self.sector_size as u64).expect("offset too large");
        let end = offset + buf.len();
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        let offset = usize::try_from(lba * self.sector_size as u64).expect("offset too large");
        let end = offset + buf.len();
        self.data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.flushed = true;
        Ok(())
    }
}
