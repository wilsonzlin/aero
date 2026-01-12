#![no_main]

use aero_storage::{AeroSparseDisk, DiskError, Result, StorageBackend, VirtualDisk};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

/// Read-only storage backend over the fuzzer-provided byte buffer.
///
/// This avoids copying the input (the backing store) while still exercising the
/// full `StorageBackend` API surface expected by `AeroSparseDisk`.
struct FuzzBackend<'a> {
    data: &'a [u8],
}

impl StorageBackend for FuzzBackend<'_> {
    fn len(&mut self) -> Result<u64> {
        Ok(self.data.len() as u64)
    }

    fn set_len(&mut self, _len: u64) -> Result<()> {
        Err(DiskError::Io("fuzz backend is read-only".into()))
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
                capacity: self.data.len() as u64,
            });
        }

        buf.copy_from_slice(&self.data[offset_usize..end]);
        Ok(())
    }

    fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> Result<()> {
        Err(DiskError::Io("fuzz backend is read-only".into()))
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    let backend = FuzzBackend { data };

    let Ok(mut disk) = AeroSparseDisk::open(backend) else {
        return;
    };

    // Drive a few bounded reads to exercise allocation table lookups and IO.
    let capacity = disk.capacity_bytes();
    const MAX_READ_LEN: usize = 4096;
    let mut scratch = vec![0u8; MAX_READ_LEN];

    let mut u = Unstructured::new(data);
    let read_ops: usize = u.int_in_range(0usize..=8).unwrap_or(0);

    for _ in 0..read_ops {
        let raw_off: u64 = u.arbitrary().unwrap_or(0);
        let raw_len: usize = u.int_in_range(0usize..=MAX_READ_LEN).unwrap_or(0);

        // Clamp the read to be within the virtual disk capacity to avoid trivially
        // failing the checked_range guards.
        let (offset, len) = if capacity == 0 {
            (0u64, 0usize)
        } else {
            let mut len = raw_len.min(MAX_READ_LEN);
            // If the disk is smaller than our max read, shrink further.
            if capacity < len as u64 {
                len = capacity as usize;
            }
            let max_off = capacity.saturating_sub(len as u64);
            (raw_off % (max_off.saturating_add(1)), len)
        };

        let _ = disk.read_at(offset, &mut scratch[..len]);
    }

    let _ = disk.flush();
});
