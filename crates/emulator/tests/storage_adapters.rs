use aero_storage::{MemBackend, RawDisk, VirtualDisk as _};

use emulator::io::storage::adapters::{EmuDiskBackendFromVirtualDisk, StorageBackendFromByteStorage};
use emulator::io::storage::disk::ByteStorage;
use emulator::io::storage::{DiskBackend as _, DiskError};

#[derive(Default, Clone)]
struct MemByteStorage {
    data: Vec<u8>,
}

impl ByteStorage for MemByteStorage {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> emulator::io::storage::DiskResult<()> {
        let offset: usize = offset.try_into().map_err(|_| DiskError::OutOfBounds)?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(DiskError::OutOfBounds)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfBounds);
        }
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> emulator::io::storage::DiskResult<()> {
        let offset: usize = offset.try_into().map_err(|_| DiskError::OutOfBounds)?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(DiskError::OutOfBounds)?;
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> emulator::io::storage::DiskResult<()> {
        Ok(())
    }

    fn len(&mut self) -> emulator::io::storage::DiskResult<u64> {
        Ok(self.data.len() as u64)
    }

    fn set_len(&mut self, len: u64) -> emulator::io::storage::DiskResult<()> {
        let len: usize = len.try_into().map_err(|_| DiskError::OutOfBounds)?;
        self.data.resize(len, 0);
        Ok(())
    }
}

#[test]
fn aero_raw_disk_wrapped_as_emulator_disk_backend() {
    let sectors: u64 = 64;
    let backend = MemBackend::new();
    let raw = RawDisk::create(backend, sectors * 512).unwrap();

    let mut emu = EmuDiskBackendFromVirtualDisk(raw);
    assert_eq!(emu.sector_size(), 512);
    assert_eq!(emu.total_sectors(), sectors);

    let mut write_buf = vec![0u8; 512 * 4];
    for (i, b) in write_buf.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }

    emu.write_sectors(10, &write_buf).unwrap();
    emu.flush().unwrap();

    let mut read_buf = vec![0u8; write_buf.len()];
    emu.read_sectors(10, &mut read_buf).unwrap();
    assert_eq!(read_buf, write_buf);

    // Error mapping: unaligned buffer length.
    let mut unaligned = [0u8; 1];
    let err = emu.read_sectors(0, &mut unaligned).unwrap_err();
    assert!(matches!(err, DiskError::UnalignedBuffer { .. }));

    // Error mapping: out-of-range.
    let mut oob = vec![0u8; 512 * 2];
    let err = emu.read_sectors(sectors - 1, &mut oob).unwrap_err();
    assert!(matches!(err, DiskError::OutOfRange { .. }));
}

#[test]
fn emulator_byte_storage_wrapped_as_aero_storage_backend() {
    let backend = StorageBackendFromByteStorage(MemByteStorage::default());
    let mut disk = RawDisk::create(backend, 16 * 512).unwrap();

    let write_buf = vec![0xA5u8; 512];
    disk.write_sectors(3, &write_buf).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = RawDisk::open(backend).unwrap();
    let mut read_buf = vec![0u8; 512];
    reopened.read_sectors(3, &mut read_buf).unwrap();
    assert_eq!(read_buf, write_buf);
}

