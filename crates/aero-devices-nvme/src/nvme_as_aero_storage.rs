use aero_storage::{DiskError as StorageDiskError, Result as StorageResult, VirtualDisk};

use crate::{DiskBackend, DiskError};

/// Adapter that exposes an NVMe [`DiskBackend`] implementation as an
/// [`aero_storage::VirtualDisk`].
///
/// This is the "reverse" of [`crate::from_virtual_disk`]/[`crate::NvmeDiskFromAeroStorage`], and
/// exists so higher-level `aero_storage` disk wrappers (cache/sparse/overlay) can be layered on top
/// of an existing NVMe backend implementation.
///
/// Note: NVMe backends are sector-addressed; this adapter only supports *sector-aligned* byte
/// offsets and lengths.
pub struct NvmeBackendAsAeroVirtualDisk {
    backend: Box<dyn DiskBackend>,
    sector_size: u32,
    capacity_bytes: u64,
}

impl NvmeBackendAsAeroVirtualDisk {
    pub fn new(backend: Box<dyn DiskBackend>) -> Result<Self, DiskError> {
        let sector_size = backend.sector_size();
        if sector_size == 0 || !sector_size.is_power_of_two() {
            return Err(DiskError::Io);
        }

        let capacity_bytes = backend
            .total_sectors()
            .checked_mul(u64::from(sector_size))
            .ok_or(DiskError::Io)?;

        Ok(Self {
            backend,
            sector_size,
            capacity_bytes,
        })
    }

    #[allow(dead_code)]
    pub fn into_inner(self) -> Box<dyn DiskBackend> {
        self.backend
    }

    fn check_aligned(&self, offset: u64, len: usize) -> StorageResult<()> {
        let alignment = self.sector_size as usize;
        let sector_size = u64::from(self.sector_size);

        if !offset.is_multiple_of(sector_size) {
            let len = usize::try_from(offset).map_err(|_| StorageDiskError::OffsetOverflow)?;
            return Err(StorageDiskError::UnalignedLength { len, alignment });
        }
        if !len.is_multiple_of(alignment) {
            return Err(StorageDiskError::UnalignedLength { len, alignment });
        }
        Ok(())
    }

    fn check_bounds(&self, offset: u64, len: usize) -> StorageResult<()> {
        let len_u64 = u64::try_from(len).map_err(|_| StorageDiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(len_u64)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        if end > self.capacity_bytes {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len,
                capacity: self.capacity_bytes,
            });
        }
        Ok(())
    }

    fn map_backend_error(&self, err: DiskError, offset: u64, len: usize) -> StorageDiskError {
        match err {
            DiskError::OutOfRange { .. } => StorageDiskError::OutOfBounds {
                offset,
                len,
                capacity: self.capacity_bytes,
            },
            DiskError::UnalignedBuffer { len, sector_size } => StorageDiskError::UnalignedLength {
                len,
                alignment: sector_size as usize,
            },
            DiskError::Io => StorageDiskError::Io("nvme backend io error".into()),
        }
    }
}

impl VirtualDisk for NvmeBackendAsAeroVirtualDisk {
    fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StorageResult<()> {
        self.check_aligned(offset, buf.len())?;
        self.check_bounds(offset, buf.len())?;

        let lba = offset / u64::from(self.sector_size);
        self.backend
            .read_sectors(lba, buf)
            .map_err(|e| self.map_backend_error(e, offset, buf.len()))
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> StorageResult<()> {
        self.check_aligned(offset, buf.len())?;
        self.check_bounds(offset, buf.len())?;

        let lba = offset / u64::from(self.sector_size);
        self.backend
            .write_sectors(lba, buf)
            .map_err(|e| self.map_backend_error(e, offset, buf.len()))
    }

    fn flush(&mut self) -> StorageResult<()> {
        self.backend
            .flush()
            .map_err(|e| self.map_backend_error(e, 0, 0))
    }

    fn discard_range(&mut self, offset: u64, len: u64) -> StorageResult<()> {
        if len == 0 {
            if offset > self.capacity_bytes {
                return Err(StorageDiskError::OutOfBounds {
                    offset,
                    len: 0,
                    capacity: self.capacity_bytes,
                });
            }
            return Ok(());
        }

        let sector_size = u64::from(self.sector_size);
        let alignment = self.sector_size as usize;

        // Enforce sector alignment (matches read/write paths).
        if !offset.is_multiple_of(sector_size) {
            let len = usize::try_from(offset).map_err(|_| StorageDiskError::OffsetOverflow)?;
            return Err(StorageDiskError::UnalignedLength { len, alignment });
        }
        if !len.is_multiple_of(sector_size) {
            let len = usize::try_from(len).map_err(|_| StorageDiskError::OffsetOverflow)?;
            return Err(StorageDiskError::UnalignedLength { len, alignment });
        }

        let end = offset
            .checked_add(len)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        let len_usize = usize::try_from(len).unwrap_or(usize::MAX);
        if end > self.capacity_bytes {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len: len_usize,
                capacity: self.capacity_bytes,
            });
        }

        let lba = offset / sector_size;
        let sectors = len / sector_size;
        self.backend
            .discard_sectors(lba, sectors)
            .map_err(|e| self.map_backend_error(e, offset, len_usize))
    }
}
