#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use aero_storage::StorageBackend;
#[cfg(not(target_arch = "wasm32"))]
use criterion::{criterion_group, criterion_main, Criterion};
#[cfg(not(target_arch = "wasm32"))]
use emulator::io::storage::cache::{BlockCache, BlockCacheConfig};
#[cfg(not(target_arch = "wasm32"))]
use emulator::io::storage::disk::{DiskBackend, WriteCachePolicy};
#[cfg(not(target_arch = "wasm32"))]
use emulator::io::storage::formats::raw::RawDisk;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct MemStorage {
    data: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl StorageBackend for MemStorage {
    fn len(&mut self) -> aero_storage::Result<u64> {
        Ok(self.data.len() as u64)
    }

    fn set_len(&mut self, len: u64) -> aero_storage::Result<()> {
        self.data.resize(len as usize, 0);
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        let offset = offset as usize;
        let end = offset + buf.len();
        if end > self.data.len() {
            return Err(aero_storage::DiskError::OutOfBounds {
                offset: offset as u64,
                len: buf.len(),
                capacity: self.data.len() as u64,
            });
        }
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        let offset = offset as usize;
        let end = offset + buf.len();
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_sequential_rw(c: &mut Criterion) {
    const SECTOR_SIZE: u32 = 512;
    const BYTES: usize = 64 * 1024 * 1024;
    let sectors = (BYTES / SECTOR_SIZE as usize) as u64;

    c.bench_function("sequential_64m_write_then_read", |b| {
        b.iter(|| {
            let storage = MemStorage::default();
            let raw = RawDisk::create(storage, SECTOR_SIZE, sectors).unwrap();
            let config =
                BlockCacheConfig::new(1024 * 1024, 8).write_policy(WriteCachePolicy::WriteBack);
            let mut disk = BlockCache::new(raw, config).unwrap();

            let mut write_buf = vec![0u8; BYTES];
            for (i, v) in write_buf.iter_mut().enumerate() {
                *v = (i as u8).wrapping_mul(31);
            }
            disk.write_sectors(0, &write_buf).unwrap();
            disk.flush().unwrap();

            let mut read_buf = vec![0u8; BYTES];
            disk.read_sectors(0, &mut read_buf).unwrap();
            assert_eq!(write_buf, read_buf);
        });
    });
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(benches, bench_sequential_rw);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
