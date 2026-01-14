use aero_storage::{Qcow2Disk as StorageQcow2Disk, VirtualDisk as _};

use crate::io::storage::adapters::{
    aero_storage_disk_error_to_emulator, aero_storage_disk_error_to_emulator_with_sector_context,
};
use crate::io::storage::disk::{DiskBackend, MaybeSend};
use crate::io::storage::error::{DiskError, DiskResult};

const SECTOR_SIZE: u32 = 512;

/// QCOW2 v2/v3 disk image backed by an [`aero_storage::StorageBackend`].
///
/// This is a thin compatibility wrapper around the canonical `aero_storage::Qcow2Disk`
/// implementation.
pub struct Qcow2Disk<S> {
    inner: StorageQcow2Disk<S>,
}

impl<S: aero_storage::StorageBackend> Qcow2Disk<S> {
    pub fn open(storage: S) -> DiskResult<Self> {
        let inner = StorageQcow2Disk::open(storage).map_err(aero_storage_disk_error_to_emulator)?;
        Ok(Self { inner })
    }

    pub fn into_storage(self) -> S {
        self.inner.into_backend()
    }
}

impl<S: aero_storage::StorageBackend + MaybeSend> DiskBackend for Qcow2Disk<S> {
    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }

    fn total_sectors(&self) -> u64 {
        self.inner.capacity_bytes() / SECTOR_SIZE as u64
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        if buf.is_empty() {
            return Ok(());
        }

        if !buf.len().is_multiple_of(SECTOR_SIZE as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: buf.len(),
                sector_size: SECTOR_SIZE,
            });
        }

        let sectors = (buf.len() / SECTOR_SIZE as usize) as u64;
        let capacity_sectors = self.total_sectors();
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors,
        })?;
        if end > capacity_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            });
        }

        self.inner.read_sectors(lba, buf).map_err(|err| {
            aero_storage_disk_error_to_emulator_with_sector_context(
                err,
                lba,
                sectors,
                capacity_sectors,
            )
        })
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        if buf.is_empty() {
            return Ok(());
        }

        if !buf.len().is_multiple_of(SECTOR_SIZE as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: buf.len(),
                sector_size: SECTOR_SIZE,
            });
        }

        let sectors = (buf.len() / SECTOR_SIZE as usize) as u64;
        let capacity_sectors = self.total_sectors();
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors,
        })?;
        if end > capacity_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            });
        }

        self.inner.write_sectors(lba, buf).map_err(|err| {
            aero_storage_disk_error_to_emulator_with_sector_context(
                err,
                lba,
                sectors,
                capacity_sectors,
            )
        })
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.inner
            .flush()
            .map_err(aero_storage_disk_error_to_emulator)
    }
}
