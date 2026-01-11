use aero_storage::{
    AeroSparseConfig, AeroSparseDisk as StorageAeroSparseDisk, AeroSparseHeader,
    DiskError as StorageDiskError, StorageBackend, VirtualDisk as _,
};

use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

const SECTOR_SIZE: u32 = 512;

struct ByteStorageBackend<S> {
    storage: S,
}

impl<S> ByteStorageBackend<S> {
    fn new(storage: S) -> Self {
        Self { storage }
    }

    fn into_storage(self) -> S {
        self.storage
    }
}

fn storage_backend_error(_err: DiskError) -> StorageDiskError {
    // `aero-storage` uses `&'static str` errors for backend I/O failures.
    //
    // The concrete error message from the emulator storage layer is not currently
    // representable, so we collapse all backend failures into a generic I/O error.
    StorageDiskError::Io("backend io error")
}

fn disk_error_from_storage(err: StorageDiskError) -> DiskError {
    match err {
        StorageDiskError::UnalignedLength { len, alignment } => DiskError::UnalignedBuffer {
            len,
            sector_size: alignment.try_into().unwrap_or(SECTOR_SIZE),
        },
        StorageDiskError::OutOfBounds { .. } => DiskError::OutOfBounds,
        StorageDiskError::OffsetOverflow => DiskError::Unsupported("offset overflow"),
        StorageDiskError::InvalidSparseHeader(msg) => DiskError::CorruptImage(msg),
        StorageDiskError::InvalidConfig(msg) => DiskError::Unsupported(msg),
        StorageDiskError::CorruptSparseImage(msg) => DiskError::CorruptImage(msg),
        StorageDiskError::Io(msg) => DiskError::Io(msg.to_string()),
    }
}

impl<S: ByteStorage> StorageBackend for ByteStorageBackend<S> {
    fn len(&mut self) -> aero_storage::Result<u64> {
        self.storage.len().map_err(storage_backend_error)
    }

    fn set_len(&mut self, len: u64) -> aero_storage::Result<()> {
        self.storage.set_len(len).map_err(storage_backend_error)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.storage
            .read_at(offset, buf)
            .map_err(storage_backend_error)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.storage
            .write_at(offset, buf)
            .map_err(storage_backend_error)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.storage.flush().map_err(storage_backend_error)
    }
}

/// Aero sparse disk format v1 (`AEROSPAR`).
pub struct AerosparDisk<S> {
    inner: StorageAeroSparseDisk<ByteStorageBackend<S>>,
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
            ByteStorageBackend::new(storage),
            AeroSparseConfig {
                disk_size_bytes,
                block_size_bytes: block_size,
            },
        )
        .map_err(disk_error_from_storage)?;

        Ok(Self { inner })
    }

    pub fn open(storage: S) -> DiskResult<Self> {
        let inner = StorageAeroSparseDisk::open(ByteStorageBackend::new(storage))
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
        self.inner.into_backend().into_storage()
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

