use aero_storage::{BlockCachedDisk, DiskError, Result, VirtualDisk};

#[derive(Debug, Clone)]
struct FaultyDisk {
    data: Vec<u8>,
    fail_writes_at: Option<u64>,
}

impl FaultyDisk {
    fn new(len: usize) -> Self {
        Self {
            data: vec![0; len],
            fail_writes_at: None,
        }
    }

    fn set_fail_writes_at(&mut self, offset: Option<u64>) {
        self.fail_writes_at = offset;
    }
}

impl VirtualDisk for FaultyDisk {
    fn capacity_bytes(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset_usize: usize = offset.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset_usize
            .checked_add(buf.len())
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity_bytes(),
            });
        }
        buf.copy_from_slice(&self.data[offset_usize..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        if self.fail_writes_at == Some(offset) {
            return Err(DiskError::Io(format!(
                "simulated write failure at offset {offset}"
            )));
        }

        let offset_usize: usize = offset.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset_usize
            .checked_add(buf.len())
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity_bytes(),
            });
        }
        self.data[offset_usize..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn block_cache_does_not_lose_dirty_block_on_eviction_writeback_error() {
    let block_size = 4usize;
    let capacity = block_size * 2;

    let mut inner = FaultyDisk::new(capacity);
    // Fail writes to block 0 to simulate an I/O error during eviction write-back.
    inner.set_fail_writes_at(Some(0));

    let mut disk = BlockCachedDisk::new(inner, block_size, 1).unwrap();

    // Dirty block 0 in the cache.
    let payload = [0xde, 0xad, 0xbe, 0xef];
    disk.write_at(0, &payload).unwrap();

    // Access block 1 to force eviction of block 0. The write-back should fail.
    let mut tmp = [0u8; 1];
    let err = disk.read_at(block_size as u64, &mut tmp).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // The dirty data for block 0 must still be readable from the cache after the failure.
    let mut readback = [0u8; 4];
    disk.read_at(0, &mut readback).unwrap();
    assert_eq!(readback, payload);

    // Once the underlying error is cleared, a flush should persist the dirty cache entry.
    disk.inner_mut().set_fail_writes_at(None);
    disk.flush().unwrap();

    let mut persisted = [0u8; 4];
    disk.inner_mut().read_at(0, &mut persisted).unwrap();
    assert_eq!(persisted, payload);
}
