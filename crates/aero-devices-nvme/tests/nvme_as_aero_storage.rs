use aero_devices_nvme::{DiskBackend, DiskError, NvmeBackendAsAeroVirtualDisk};
use aero_storage::{DiskError as StorageDiskError, VirtualDisk};

#[derive(Debug)]
struct MemDiskBackend {
    sector_size: u32,
    data: Vec<u8>,
}

impl MemDiskBackend {
    fn new(sector_size: u32, total_sectors: u64) -> Self {
        let capacity_bytes = usize::try_from(total_sectors)
            .unwrap()
            .checked_mul(sector_size as usize)
            .unwrap();
        Self {
            sector_size,
            data: vec![0; capacity_bytes],
        }
    }
}

impl DiskBackend for MemDiskBackend {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        (self.data.len() as u64) / u64::from(self.sector_size)
    }

    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), DiskError> {
        let sector_size = self.sector_size as usize;
        if !buffer.len().is_multiple_of(sector_size) {
            return Err(DiskError::UnalignedBuffer {
                len: buffer.len(),
                sector_size: self.sector_size,
            });
        }
        let sectors = (buffer.len() / sector_size) as u64;
        let end_lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors(),
        })?;
        let cap = self.total_sectors();
        if end_lba > cap {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: cap,
            });
        }

        let start = (lba as usize) * sector_size;
        let end = start + buffer.len();
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> Result<(), DiskError> {
        let sector_size = self.sector_size as usize;
        if !buffer.len().is_multiple_of(sector_size) {
            return Err(DiskError::UnalignedBuffer {
                len: buffer.len(),
                sector_size: self.sector_size,
            });
        }
        let sectors = (buffer.len() / sector_size) as u64;
        let end_lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors(),
        })?;
        let cap = self.total_sectors();
        if end_lba > cap {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: cap,
            });
        }

        let start = (lba as usize) * sector_size;
        let end = start + buffer.len();
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        Ok(())
    }

    fn discard_sectors(&mut self, lba: u64, sectors: u64) -> Result<(), DiskError> {
        if sectors == 0 {
            return Ok(());
        }

        let sector_size = self.sector_size as usize;
        let len_bytes = sectors
            .checked_mul(u64::from(self.sector_size))
            .ok_or(DiskError::Io)? as usize;
        let end_lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors(),
        })?;
        let cap = self.total_sectors();
        if end_lba > cap {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: cap,
            });
        }

        let start = (lba as usize) * sector_size;
        let end = start + len_bytes;
        self.data[start..end].fill(0);
        Ok(())
    }
}

#[test]
fn nvme_backend_as_aero_virtual_disk_read_write_roundtrip() {
    let backend = Box::new(MemDiskBackend::new(512, 4));
    let mut disk = NvmeBackendAsAeroVirtualDisk::new(backend).unwrap();
    assert_eq!(disk.capacity_bytes(), 4 * 512);

    let payload: Vec<u8> = (0..(2 * 512)).map(|i| (i & 0xff) as u8).collect();
    disk.write_at(512, &payload).unwrap();

    let mut out = vec![0u8; payload.len()];
    disk.read_at(512, &mut out).unwrap();
    assert_eq!(out, payload);
}

#[test]
fn nvme_backend_as_aero_virtual_disk_rejects_unaligned_read_write() {
    let backend = Box::new(MemDiskBackend::new(512, 4));
    let mut disk = NvmeBackendAsAeroVirtualDisk::new(backend).unwrap();

    let mut buf = vec![0u8; 512];
    let err = disk.read_at(1, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        StorageDiskError::UnalignedLength {
            len: 1,
            alignment: 512
        }
    ));

    let payload = vec![0u8; 513];
    let err = disk.write_at(0, &payload).unwrap_err();
    assert!(matches!(
        err,
        StorageDiskError::UnalignedLength {
            len: 513,
            alignment: 512
        }
    ));
}

#[test]
fn nvme_backend_as_aero_virtual_disk_maps_out_of_range_to_out_of_bounds() {
    let backend = Box::new(MemDiskBackend::new(512, 2));
    let mut disk = NvmeBackendAsAeroVirtualDisk::new(backend).unwrap();
    let cap = disk.capacity_bytes();
    assert_eq!(cap, 2 * 512);

    let mut buf = vec![0u8; 512];
    let err = disk.read_at(cap, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        StorageDiskError::OutOfBounds {
            offset,
            len: 512,
            capacity
        } if offset == cap && capacity == cap
    ));

    let payload = vec![0u8; 512];
    let err = disk.write_at(cap, &payload).unwrap_err();
    assert!(matches!(
        err,
        StorageDiskError::OutOfBounds {
            offset,
            len: 512,
            capacity
        } if offset == cap && capacity == cap
    ));
}

#[test]
fn nvme_backend_as_aero_virtual_disk_discard_range_forwards_to_backend() {
    let backend = Box::new(MemDiskBackend::new(512, 4));
    let mut disk = NvmeBackendAsAeroVirtualDisk::new(backend).unwrap();

    let payload = vec![0x5Au8; 512];
    disk.write_at(0, &payload).unwrap();

    let mut out = vec![0u8; 512];
    disk.read_at(0, &mut out).unwrap();
    assert_eq!(out, payload);

    disk.discard_range(0, 512).unwrap();

    disk.read_at(0, &mut out).unwrap();
    assert_eq!(out, vec![0u8; 512]);
}
