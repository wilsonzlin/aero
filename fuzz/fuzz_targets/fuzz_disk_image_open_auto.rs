#![no_main]

use aero_storage::{DiskError, DiskImage, Result, StorageBackend, VirtualDisk};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 1024 * 1024; // 1 MiB
const MAX_READ_LEN: usize = 4096; // 4 KiB
// Keep offsets near the start of the disk so very large virtual sizes (common for sparse formats)
// still exercise mapping logic instead of immediately erroring on huge indices.
const MAX_TOUCHED_CAP_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB

/// Read-only `StorageBackend` over the fuzzer-provided byte buffer.
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
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let backend = FuzzBackend { data };
    let Ok(mut disk) = DiskImage::open_auto(backend) else {
        return;
    };

    let cap = disk.capacity_bytes();
    let touched_cap = cap.min(MAX_TOUCHED_CAP_BYTES);
    let mut scratch = [0u8; MAX_READ_LEN];

    // Deterministic boundary reads to ensure we exercise I/O even if the fuzzer chooses 0 ops.
    if cap > 0 {
        let _ = disk.read_at(0, &mut scratch[..1]);
        let _ = disk.read_at(cap - 1, &mut scratch[..1]);
    }
    if touched_cap > 0 && touched_cap != cap {
        let _ = disk.read_at(touched_cap - 1, &mut scratch[..1]);
    }

    let mut u = Unstructured::new(data);
    let ops: usize = u.int_in_range(0usize..=8).unwrap_or(0);
    for _ in 0..ops {
        let raw_off: u64 = u.arbitrary().unwrap_or(0);
        let raw_len: usize = u.int_in_range(0usize..=MAX_READ_LEN).unwrap_or(0);
        if raw_len == 0 {
            continue;
        }

        // Clamp within a small window at the start of the disk to avoid trivially failing
        // huge-index guards in formats with very large virtual sizes.
        let len = if touched_cap < raw_len as u64 {
            touched_cap as usize
        } else {
            raw_len
        };
        let max_off = touched_cap.saturating_sub(len as u64);
        let off = if touched_cap == 0 {
            0
        } else {
            raw_off % (max_off.saturating_add(1))
        };

        let _ = disk.read_at(off, &mut scratch[..len]);
    }

    let _ = disk.flush();
});
