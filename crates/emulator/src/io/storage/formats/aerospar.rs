use aero_storage::{
    AeroSparseConfig, AeroSparseDisk as StorageAeroSparseDisk, AeroSparseHeader,
    DiskError as StorageDiskError, VirtualDisk as _,
};

use crate::io::storage::adapters::StorageBackendFromByteStorage;
use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

const SECTOR_SIZE: u32 = 512;

fn disk_error_from_storage(err: StorageDiskError) -> DiskError {
    match err {
        StorageDiskError::UnalignedLength { len, alignment } => DiskError::UnalignedBuffer {
            len,
            sector_size: alignment.try_into().unwrap_or(SECTOR_SIZE),
        },
        StorageDiskError::OutOfBounds { .. } => DiskError::OutOfBounds,
        StorageDiskError::OffsetOverflow => DiskError::Unsupported("offset overflow"),
        StorageDiskError::CorruptImage(msg) => DiskError::CorruptImage(msg),
        StorageDiskError::Unsupported(msg) => DiskError::Unsupported(msg),
        StorageDiskError::InvalidSparseHeader(msg) => DiskError::CorruptImage(msg),
        StorageDiskError::InvalidConfig(msg) => DiskError::Unsupported(msg),
        StorageDiskError::CorruptSparseImage(msg) => DiskError::CorruptImage(msg),
        StorageDiskError::NotSupported(msg) => DiskError::NotSupported(msg),
        StorageDiskError::QuotaExceeded => DiskError::QuotaExceeded,
        StorageDiskError::InUse => DiskError::InUse,
        StorageDiskError::InvalidState(msg) => DiskError::InvalidState(msg),
        StorageDiskError::BackendUnavailable => DiskError::BackendUnavailable,
        StorageDiskError::Io(msg) => DiskError::Io(msg),
    }
}

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
            StorageBackendFromByteStorage(storage),
            AeroSparseConfig {
                disk_size_bytes,
                block_size_bytes: block_size,
            },
        )
        .map_err(disk_error_from_storage)?;

        Ok(Self { inner })
    }

    pub fn open(storage: S) -> DiskResult<Self> {
        let inner = StorageAeroSparseDisk::open(StorageBackendFromByteStorage(storage))
            .map_err(disk_error_from_storage)?;

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
            .map_err(disk_error_from_storage)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.inner
            .write_sectors(lba, buf)
            .map_err(disk_error_from_storage)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.inner.flush().map_err(disk_error_from_storage)
    }
}
