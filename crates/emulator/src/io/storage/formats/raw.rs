use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

pub struct RawDisk<S> {
    storage: S,
    sector_size: u32,
    total_sectors: u64,
}

impl<S: ByteStorage> RawDisk<S> {
    pub fn create(mut storage: S, sector_size: u32, total_sectors: u64) -> DiskResult<Self> {
        if sector_size == 0 {
            return Err(DiskError::Unsupported("sector size must be non-zero"));
        }
        let len = total_sectors
            .checked_mul(sector_size as u64)
            .ok_or(DiskError::Unsupported("disk size overflow"))?;
        storage.set_len(len)?;
        Ok(Self {
            storage,
            sector_size,
            total_sectors,
        })
    }

    pub fn open(mut storage: S, sector_size: u32) -> DiskResult<Self> {
        if sector_size == 0 {
            return Err(DiskError::Unsupported("sector size must be non-zero"));
        }
        let len = storage.len()?;
        if !len.is_multiple_of(sector_size as u64) {
            return Err(DiskError::CorruptImage(
                "raw size not multiple of sector size",
            ));
        }
        Ok(Self {
            storage,
            sector_size,
            total_sectors: len / sector_size as u64,
        })
    }

    pub fn into_storage(self) -> S {
        self.storage
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
        let sectors = self.check_range(lba, buf.len())?;
        let offset = lba.checked_mul(self.sector_size as u64).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors,
        })?;
        self.storage.read_at(offset, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        let sectors = self.check_range(lba, buf.len())?;
        let offset = lba.checked_mul(self.sector_size as u64).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors,
        })?;
        self.storage.write_at(offset, buf)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.storage.flush()
    }
}
