use emulator::io::storage::disk::ByteStorage;
use emulator::io::storage::formats::SparseDisk;
use emulator::io::storage::{DiskBackend, DiskResult};
use proptest::prelude::*;

#[derive(Default, Clone)]
struct MemStorage {
    data: Vec<u8>,
}

impl ByteStorage for MemStorage {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
        let offset = offset as usize;
        let end = offset + buf.len();
        if end > self.data.len() {
            return Err(emulator::io::storage::DiskError::Io("read past end".into()));
        }
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()> {
        let offset = offset as usize;
        let end = offset + buf.len();
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        Ok(())
    }

    fn len(&mut self) -> DiskResult<u64> {
        Ok(self.data.len() as u64)
    }

    fn set_len(&mut self, len: u64) -> DiskResult<()> {
        self.data.resize(len as usize, 0);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct WriteOp {
    lba: u64,
    data: Vec<u8>,
}

const SECTOR_SIZE: u32 = 512;
const TOTAL_SECTORS: u64 = 1024;
const BLOCK_SIZE: u32 = 8 * 1024;
const MAX_SECTORS_PER_OP: u64 = 32;

fn write_op_strategy() -> impl Strategy<Value = WriteOp> {
    (0u64..TOTAL_SECTORS).prop_flat_map(|lba| {
        let max = (TOTAL_SECTORS - lba).min(MAX_SECTORS_PER_OP).max(1);
        (Just(lba), 1u64..=max).prop_flat_map(|(lba, sectors)| {
            let len = sectors as usize * SECTOR_SIZE as usize;
            prop::collection::vec(any::<u8>(), len).prop_map(move |data| WriteOp { lba, data })
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]
    #[test]
    fn sparse_matches_reference_after_random_writes(ops in prop::collection::vec(write_op_strategy(), 1..30)) {
        let storage = MemStorage::default();
        let mut disk = SparseDisk::create(storage, SECTOR_SIZE, TOTAL_SECTORS, BLOCK_SIZE).unwrap();

        let mut reference = vec![0u8; TOTAL_SECTORS as usize * SECTOR_SIZE as usize];

        for op in &ops {
            let offset = op.lba as usize * SECTOR_SIZE as usize;
            reference[offset..offset + op.data.len()].copy_from_slice(&op.data);
            disk.write_sectors(op.lba, &op.data).unwrap();

            let mut read_back = vec![0u8; op.data.len()];
            disk.read_sectors(op.lba, &mut read_back).unwrap();
            prop_assert_eq!(read_back.as_slice(), op.data.as_slice());
        }

        disk.flush().unwrap();
        let storage = disk.into_storage();

        let mut reopened = SparseDisk::open(storage).unwrap();
        let mut all = vec![0u8; reference.len()];
        reopened.read_sectors(0, &mut all).unwrap();
        prop_assert_eq!(all, reference);
    }
}
