#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskError {
    OutOfBounds,
    InvalidBufferLength,
}

pub type DiskResult<T> = Result<T, DiskError>;

/// A simple sector-addressable disk abstraction used by the AHCI controller.
pub trait DiskBackend {
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()>;
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()>;
    fn flush(&mut self) -> DiskResult<()>;
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
        let len = usize::try_from(total_sectors * sector_size as u64).unwrap();
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
}

impl DiskBackend for MemDisk {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        (self.data.len() as u64) / self.sector_size as u64
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        if buf.len() as u64 % self.sector_size as u64 != 0 {
            return Err(DiskError::InvalidBufferLength);
        }
        let offset = lba * self.sector_size as u64;
        let end = offset + buf.len() as u64;
        let end = usize::try_from(end).unwrap();
        let offset = usize::try_from(offset).unwrap();
        if end > self.data.len() {
            return Err(DiskError::OutOfBounds);
        }
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        if buf.len() as u64 % self.sector_size as u64 != 0 {
            return Err(DiskError::InvalidBufferLength);
        }
        let offset = lba * self.sector_size as u64;
        let end = offset + buf.len() as u64;
        let end = usize::try_from(end).unwrap();
        let offset = usize::try_from(offset).unwrap();
        if end > self.data.len() {
            return Err(DiskError::OutOfBounds);
        }
        self.data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.flushed = true;
        Ok(())
    }
}
