use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, MemBackend, StorageBackend as _, VirtualDisk as _,
};
use emulator::io::storage::disk::ByteStorage;
use emulator::io::storage::{DiskBackend as _, DiskFormat, VirtualDrive, WriteCachePolicy};

#[derive(Default, Clone)]
struct MemStorage {
    data: Vec<u8>,
}

impl MemStorage {
    fn from_bytes(data: Vec<u8>) -> Self {
        Self { data }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

impl ByteStorage for MemStorage {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> emulator::io::storage::DiskResult<()> {
        let offset = offset as usize;
        let end = offset + buf.len();
        if end > self.data.len() {
            return Err(emulator::io::storage::DiskError::Io("read past end".into()));
        }
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> emulator::io::storage::DiskResult<()> {
        let offset = offset as usize;
        let end = offset + buf.len();
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
        self.data.resize(len as usize, 0);
        Ok(())
    }
}

fn fill_deterministic(buf: &mut [u8], seed: u32) {
    let mut x = seed;
    for b in buf {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = (x & 0xff) as u8;
    }
}

#[test]
fn emulator_opens_aero_storage_aerospar() {
    let backend = MemBackend::new();
    let mut disk = AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024 * 1024,
            block_size_bytes: 1024 * 1024,
        },
    )
    .unwrap();

    let mut write_buf = vec![0u8; 4096];
    fill_deterministic(&mut write_buf, 0x1234_5678);
    disk.write_sectors(11, &write_buf).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let bytes = backend.as_slice().to_vec();

    let storage = MemStorage::from_bytes(bytes);
    let mut drive = VirtualDrive::open_auto(storage, 512, WriteCachePolicy::WriteThrough).unwrap();
    assert_eq!(drive.format(), DiskFormat::Sparse);

    let mut read_buf = vec![0u8; write_buf.len()];
    drive.read_sectors(11, &mut read_buf).unwrap();
    assert_eq!(read_buf, write_buf);
}

#[test]
fn aero_storage_opens_emulator_aerospar() {
    let storage = MemStorage::default();
    let mut disk =
        emulator::io::storage::formats::SparseDisk::create(storage, 512, 32 * 1024, 1024 * 1024)
            .unwrap();

    let mut write_buf = vec![0u8; 8192];
    fill_deterministic(&mut write_buf, 0xA5A5_5A5A);
    disk.write_sectors(123, &write_buf).unwrap();
    disk.flush().unwrap();

    let bytes = disk.into_storage().into_bytes();
    let mut backend = MemBackend::new();
    backend.write_at(0, &bytes).unwrap();

    let mut opened = AeroSparseDisk::open(backend).unwrap();
    let mut read_buf = vec![0u8; write_buf.len()];
    opened.read_sectors(123, &mut read_buf).unwrap();
    assert_eq!(read_buf, write_buf);
}

#[test]
fn emulator_aerospar_roundtrip() {
    let storage = MemStorage::default();
    let mut disk =
        emulator::io::storage::formats::SparseDisk::create(storage, 512, 16 * 1024, 1024 * 1024)
            .unwrap();

    let mut write_buf = vec![0u8; 4096];
    fill_deterministic(&mut write_buf, 0xDEAD_BEEF);
    disk.write_sectors(5, &write_buf).unwrap();
    disk.flush().unwrap();

    let storage = disk.into_storage();
    let mut reopened = emulator::io::storage::formats::SparseDisk::open(storage).unwrap();
    let mut read_buf = vec![0u8; write_buf.len()];
    reopened.read_sectors(5, &mut read_buf).unwrap();
    assert_eq!(read_buf, write_buf);
}
