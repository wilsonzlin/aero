use criterion::{criterion_group, criterion_main, Criterion};
use emulator::io::storage::cache::{BlockCache, BlockCacheConfig};
use emulator::io::storage::disk::{ByteStorage, DiskBackend, WriteCachePolicy};
use emulator::io::storage::formats::raw::RawDisk;

#[derive(Default)]
struct MemStorage {
    data: Vec<u8>,
}

impl ByteStorage for MemStorage {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> emulator::io::storage::DiskResult<()> {
        let offset = offset as usize;
        let end = offset + buf.len();
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

criterion_group!(benches, bench_sequential_rw);
criterion_main!(benches);
