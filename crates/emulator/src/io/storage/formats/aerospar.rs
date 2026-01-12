use aero_storage::{
    AeroSparseConfig, AeroSparseDisk as StorageAeroSparseDisk, AeroSparseHeader, VirtualDisk as _,
};

use crate::io::storage::adapters::{
    aero_storage_disk_error_to_emulator, StorageBackendFromByteStorage,
};
use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

const SECTOR_SIZE: u32 = 512;

/// Aero sparse disk format v1 (`AEROSPAR`).
pub struct AerosparDisk<S> {
    inner: StorageAeroSparseDisk<StorageBackendFromByteStorage<S>>,
}

impl<S: ByteStorage> AerosparDisk<S> {
    pub fn create(
        storage: S,
        sector_size: u32,
        total_sectors: u64,
        block_size: u32,
    ) -> DiskResult<Self> {
        if sector_size != SECTOR_SIZE {
            return Err(DiskError::Unsupported(
                "aerospar disks only support 512-byte sectors",
            ));
        }

        let disk_size_bytes = total_sectors
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::Unsupported("disk size overflow"))?;

        let inner = StorageAeroSparseDisk::create(
            StorageBackendFromByteStorage::new(storage),
            AeroSparseConfig {
                disk_size_bytes,
                block_size_bytes: block_size,
            },
        )
        .map_err(aero_storage_disk_error_to_emulator)?;

        Ok(Self { inner })
    }

    pub fn open(storage: S) -> DiskResult<Self> {
        let inner = StorageAeroSparseDisk::open(StorageBackendFromByteStorage::new(storage))
            .map_err(aero_storage_disk_error_to_emulator)?;

        if inner.header().disk_size_bytes % SECTOR_SIZE as u64 != 0 {
            return Err(DiskError::CorruptImage(
                "aerospar disk size is not a multiple of 512 bytes",
            ));
        }

        Ok(Self { inner })
    }

    pub fn header(&self) -> &AeroSparseHeader {
        self.inner.header()
    }

    pub fn is_block_allocated(&self, block_idx: u64) -> bool {
        self.inner.is_block_allocated(block_idx)
    }

    pub fn into_storage(self) -> S {
        self.inner.into_backend().into_inner()
    }
}

impl<S: ByteStorage> DiskBackend for AerosparDisk<S> {
    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }

    fn total_sectors(&self) -> u64 {
        self.inner.capacity_bytes() / SECTOR_SIZE as u64
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.inner
            .read_sectors(lba, buf)
            .map_err(aero_storage_disk_error_to_emulator)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.inner
            .write_sectors(lba, buf)
            .map_err(aero_storage_disk_error_to_emulator)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.inner
            .flush()
            .map_err(aero_storage_disk_error_to_emulator)
    }
}
