use aero_storage::{RawDisk as StorageRawDisk, VirtualDisk as _};

use crate::io::storage::adapters::{
    aero_storage_disk_error_to_emulator, aero_storage_disk_error_to_emulator_with_sector_context,
    StorageBackendFromByteStorage,
};
use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

/// Raw disk image backed by a byte-addressed storage primitive.
///
/// This is a thin compatibility wrapper around the canonical `aero_storage::RawDisk`
/// implementation, preserving the emulator disk stack's configurable sector size.
pub struct RawDisk<S> {
    inner: StorageRawDisk<StorageBackendFromByteStorage<S>>,
    sector_size: u32,
    total_sectors: u64,
}

impl<S: ByteStorage> RawDisk<S> {
    pub fn create(storage: S, sector_size: u32, total_sectors: u64) -> DiskResult<Self> {
        if sector_size == 0 {
            return Err(DiskError::Unsupported("sector size must be non-zero"));
        }

        let capacity_bytes = total_sectors
            .checked_mul(sector_size as u64)
            .ok_or(DiskError::Unsupported("disk size overflow"))?;

        let inner =
            StorageRawDisk::create(StorageBackendFromByteStorage::new(storage), capacity_bytes)
                .map_err(aero_storage_disk_error_to_emulator)?;

        Ok(Self {
            inner,
            sector_size,
            total_sectors,
        })
    }

    pub fn open(storage: S, sector_size: u32) -> DiskResult<Self> {
        if sector_size == 0 {
            return Err(DiskError::Unsupported("sector size must be non-zero"));
        }

        let inner = StorageRawDisk::open(StorageBackendFromByteStorage::new(storage))
            .map_err(aero_storage_disk_error_to_emulator)?;

        let len = inner.capacity_bytes();
        if !len.is_multiple_of(sector_size as u64) {
            return Err(DiskError::CorruptImage(
                "raw size not multiple of sector size",
            ));
        }

        Ok(Self {
            total_sectors: len / sector_size as u64,
            inner,
            sector_size,
        })
    }

    pub fn into_storage(self) -> S {
        self.inner.into_backend().into_inner()
    }

    fn check_range(&self, lba: u64, bytes: usize) -> DiskResult<u64> {
        if !bytes.is_multiple_of(self.sector_size as usize) {
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

impl<S: ByteStorage> DiskBackend for RawDisk<S> {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        if buf.is_empty() {
            return Ok(());
        }

        let sectors = self.check_range(lba, buf.len())?;
        let offset = lba
            .checked_mul(self.sector_size as u64)
            .ok_or(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.total_sectors,
            })?;

        self.inner.read_at(offset, buf).map_err(|err| {
            aero_storage_disk_error_to_emulator_with_sector_context(
                err,
                lba,
                sectors,
                self.total_sectors,
            )
        })
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        if buf.is_empty() {
            return Ok(());
        }

        let sectors = self.check_range(lba, buf.len())?;
        let offset = lba
            .checked_mul(self.sector_size as u64)
            .ok_or(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.total_sectors,
            })?;

        self.inner.write_at(offset, buf).map_err(|err| {
            aero_storage_disk_error_to_emulator_with_sector_context(
                err,
                lba,
                sectors,
                self.total_sectors,
            )
        })
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.inner
            .flush()
            .map_err(aero_storage_disk_error_to_emulator)
    }
}
