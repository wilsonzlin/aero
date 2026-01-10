#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskError {
    OutOfRange,
}

pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), DiskError>;

    fn size_in_sectors(&self) -> u64;
}

#[derive(Debug, Clone)]
pub struct InMemoryDisk {
    data: Vec<u8>,
}

impl InMemoryDisk {
    pub fn new(mut data: Vec<u8>) -> Self {
        if data.len() % 512 != 0 {
            let new_len = (data.len() + 511) & !511;
            data.resize(new_len, 0);
        }
        Self { data }
    }

    pub fn from_boot_sector(sector: [u8; 512]) -> Self {
        Self {
            data: sector.to_vec(),
        }
    }
}

impl BlockDevice for InMemoryDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), DiskError> {
        let start = lba.checked_mul(512).ok_or(DiskError::OutOfRange)? as usize;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfRange);
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        (self.data.len() / 512) as u64
    }
}
