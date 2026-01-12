use aero_storage::{DiskError as StorageDiskError, VirtualDisk, SECTOR_SIZE};

use crate::{DiskBackend, DiskError, DiskResult};

/// Adapter that exposes an [`aero_storage::VirtualDisk`] as an NVMe [`DiskBackend`].
///
/// NVMe is sector-addressed. `aero-storage` disks are byte-addressed but use 512-byte sectors for
/// their `read_sectors`/`write_sectors` helpers. This adapter enforces a fixed 512-byte sector size
/// and reports total sectors as `capacity_bytes / 512`.
#[derive(Debug)]
pub struct NvmeDiskFromAeroStorage<D> {
    disk: D,
    total_sectors: u64,
}

impl<D: VirtualDisk> NvmeDiskFromAeroStorage<D> {
    pub fn new(disk: D) -> DiskResult<Self> {
        let capacity_bytes = disk.capacity_bytes();
        if !capacity_bytes.is_multiple_of(SECTOR_SIZE as u64) {
            // NVMe Identify Namespace reports capacity in sectors; reject disks that cannot be
            // represented losslessly as a 512-byte LBA device.
            return Err(DiskError::Io);
        }

        Ok(Self {
            disk,
            total_sectors: capacity_bytes / SECTOR_SIZE as u64,
        })
    }

    #[cfg(test)]
    pub fn into_inner(self) -> D {
        self.disk
    }
}

impl<D: VirtualDisk + Send> DiskBackend for NvmeDiskFromAeroStorage<D> {
    fn sector_size(&self) -> u32 {
        SECTOR_SIZE as u32
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> DiskResult<()> {
        self.disk
            .read_sectors(lba, buffer)
            .map_err(|e| map_storage_error(e, lba, buffer.len(), self.total_sectors))
    }

    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> DiskResult<()> {
        self.disk
            .write_sectors(lba, buffer)
            .map_err(|e| map_storage_error(e, lba, buffer.len(), self.total_sectors))
    }

    fn flush(&mut self) -> DiskResult<()> {
        // Surface any storage-layer error as a generic NVMe I/O error, but keep the match
        // exhaustive (via `crate::map_storage_error_to_nvme`) so new `aero_storage::DiskError`
        // variants require an explicit decision here.
        self.disk.flush().map_err(crate::map_storage_error_to_nvme)
    }
}

fn map_storage_error(
    err: StorageDiskError,
    lba: u64,
    buffer_len: usize,
    capacity_sectors: u64,
) -> DiskError {
    match err {
        StorageDiskError::UnalignedLength { len, .. } => DiskError::UnalignedBuffer {
            len,
            sector_size: SECTOR_SIZE as u32,
        },
        StorageDiskError::OutOfBounds { .. } | StorageDiskError::OffsetOverflow => {
            // `aero-storage` reports out-of-bounds at the byte level; re-express in LBA terms.
            let sectors = (buffer_len / SECTOR_SIZE) as u64;
            DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            }
        }
        // Sparse/cow/cache formats may surface structured errors; NVMe consumers treat backend
        // errors as opaque I/O failures.
        StorageDiskError::CorruptImage(_)
        | StorageDiskError::Unsupported(_)
        | StorageDiskError::InvalidSparseHeader(_)
        | StorageDiskError::InvalidConfig(_)
        | StorageDiskError::CorruptSparseImage(_)
        | StorageDiskError::NotSupported(_)
        | StorageDiskError::QuotaExceeded
        | StorageDiskError::InUse
        | StorageDiskError::InvalidState(_)
        | StorageDiskError::BackendUnavailable
        | StorageDiskError::Io(_) => DiskError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_storage::{MemBackend, RawDisk};

    #[test]
    fn nvme_adapter_rw_roundtrip_updates_rawdisk_backend() {
        let capacity_bytes = (2 * SECTOR_SIZE) as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();

        let lba = 1u64;
        let payload: Vec<u8> = (0..SECTOR_SIZE).map(|i| (i & 0xff) as u8).collect();

        let mut adapter = NvmeDiskFromAeroStorage::new(disk).unwrap();
        assert_eq!(adapter.sector_size(), 512);
        assert_eq!(adapter.total_sectors(), 2);

        adapter.write_sectors(lba, &payload).unwrap();

        let mut read_back = vec![0u8; SECTOR_SIZE];
        adapter.read_sectors(lba, &mut read_back).unwrap();
        assert_eq!(read_back, payload);

        // Verify the underlying RawDisk<MemBackend> contents changed.
        let disk = adapter.into_inner();
        let backend = disk.into_backend();
        let off = (lba as usize) * SECTOR_SIZE;
        assert_eq!(&backend.as_slice()[off..off + SECTOR_SIZE], payload.as_slice());
    }

    #[test]
    fn nvme_adapter_rejects_unaligned_capacity() {
        // `aero_storage::RawDisk` can represent any byte length, but NVMe exposes capacity in
        // whole 512-byte LBAs. The adapter must reject disks with a trailing partial sector.
        let disk = RawDisk::create(MemBackend::new(), 513).unwrap();
        assert!(matches!(
            NvmeDiskFromAeroStorage::new(disk),
            Err(DiskError::Io)
        ));
    }

    #[test]
    fn nvme_adapter_maps_unaligned_buffer() {
        let disk = RawDisk::create(MemBackend::new(), (2 * SECTOR_SIZE) as u64).unwrap();
        let mut adapter = NvmeDiskFromAeroStorage::new(disk).unwrap();

        let mut buf = vec![0u8; SECTOR_SIZE + 1];
        let err = adapter.read_sectors(0, &mut buf).unwrap_err();
        assert_eq!(
            err,
            DiskError::UnalignedBuffer {
                len: SECTOR_SIZE + 1,
                sector_size: 512
            }
        );
    }

    #[test]
    fn nvme_adapter_maps_out_of_range() {
        let disk = RawDisk::create(MemBackend::new(), (2 * SECTOR_SIZE) as u64).unwrap();
        let mut adapter = NvmeDiskFromAeroStorage::new(disk).unwrap();

        let mut buf = vec![0u8; SECTOR_SIZE];
        let err = adapter.read_sectors(2, &mut buf).unwrap_err();
        assert_eq!(
            err,
            DiskError::OutOfRange {
                lba: 2,
                sectors: 1,
                capacity_sectors: 2
            }
        );
    }

    #[test]
    fn nvme_adapter_maps_other_storage_errors_to_io() {
        struct FaultyDisk;

        impl VirtualDisk for FaultyDisk {
            fn capacity_bytes(&self) -> u64 {
                SECTOR_SIZE as u64
            }

            fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> aero_storage::Result<()> {
                Err(StorageDiskError::CorruptImage("bad"))
            }

            fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
                Err(StorageDiskError::Unsupported("feature"))
            }

            fn flush(&mut self) -> aero_storage::Result<()> {
                Err(StorageDiskError::InvalidConfig("bad config"))
            }
        }

        let mut adapter = NvmeDiskFromAeroStorage::new(FaultyDisk).unwrap();
        let mut buf = vec![0u8; SECTOR_SIZE];
        assert_eq!(adapter.read_sectors(0, &mut buf).unwrap_err(), DiskError::Io);
        assert_eq!(adapter.write_sectors(0, &buf).unwrap_err(), DiskError::Io);
        assert_eq!(adapter.flush().unwrap_err(), DiskError::Io);
    }
}
