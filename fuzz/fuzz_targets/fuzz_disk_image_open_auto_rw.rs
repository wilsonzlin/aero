#![no_main]

use aero_storage::{DiskError, DiskImage, MemBackend, Result, StorageBackend, VirtualDisk};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 1024 * 1024; // 1 MiB
const MAX_IO_BYTES: usize = 4096; // 4 KiB
const MAX_OPS: usize = 32;

// Keep offsets near the start of the disk so very large virtual sizes (common for sparse formats)
// still exercise mapping logic instead of immediately erroring on huge indices.
const MAX_TOUCHED_CAP_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB

// Hard cap to keep backend growth limited even if a parsed image claims a huge virtual size.
const MAX_BACKEND_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB

/// Wrapper around `MemBackend` that refuses to grow beyond `MAX_BACKEND_BYTES`.
///
/// This keeps the fuzzer lightweight even if the parsed image implies extreme allocations on
/// write (e.g. very large VHD blocks or QCOW2 cluster growth).
struct CappedBackend {
    inner: MemBackend,
}

impl StorageBackend for CappedBackend {
    fn len(&mut self) -> Result<u64> {
        self.inner.len()
    }

    fn set_len(&mut self, len: u64) -> Result<()> {
        if len > MAX_BACKEND_BYTES {
            return Err(DiskError::QuotaExceeded);
        }
        self.inner.set_len(len)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let len_u64 = u64::try_from(buf.len()).map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset.checked_add(len_u64).ok_or(DiskError::OffsetOverflow)?;
        if end > MAX_BACKEND_BYTES {
            return Err(DiskError::QuotaExceeded);
        }
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Back the disk image with an in-memory backend so we can exercise write paths.
    //
    // Align to 512 bytes to make it more likely VHD images are considered well-formed.
    let mut backend = MemBackend::new();
    let initial_len = ((data.len() as u64).max(512).saturating_add(511) / 512) * 512;
    let _ = backend.set_len(initial_len);
    let _ = backend.write_at(0, data);

    let Ok(mut disk) = DiskImage::open_auto(CappedBackend { inner: backend }) else {
        return;
    };

    let cap = disk.capacity_bytes();
    if cap == 0 {
        return;
    }
    let touched_cap = cap.min(MAX_TOUCHED_CAP_BYTES);

    let mut u = Unstructured::new(data);
    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);

    let mut scratch = [0u8; MAX_IO_BYTES];

    // Deterministic small I/O so we exercise the core read/write logic even if the fuzzer chooses
    // 0 randomized ops.
    if touched_cap > 0 {
        let _ = disk.read_at(0, &mut scratch[..1]);
        scratch[0] = data.first().copied().unwrap_or(0).wrapping_add(1);
        let _ = disk.write_at(0, &scratch[..1]);
        let _ = disk.read_at(touched_cap - 1, &mut scratch[..1]);
    }

    for _ in 0..ops {
        let is_write: bool = u.arbitrary().unwrap_or(false);
        let len: usize = u.int_in_range(0usize..=MAX_IO_BYTES).unwrap_or(0);
        if len == 0 {
            continue;
        }
        if touched_cap < len as u64 {
            continue;
        }

        let max_off = touched_cap - len as u64;
        let raw_off: u64 = u.arbitrary().unwrap_or(0);
        let off = if max_off == u64::MAX {
            raw_off
        } else {
            raw_off % (max_off + 1)
        };

        if is_write {
            for b in &mut scratch[..len] {
                *b = u.arbitrary().unwrap_or(0u8);
            }
            let _ = disk.write_at(off, &scratch[..len]);
        } else {
            let _ = disk.read_at(off, &mut scratch[..len]);
        }
    }

    let _ = disk.flush();

    // Re-open after mutation to exercise parsing of updated metadata.
    let backend = disk.into_backend();
    if let Ok(mut reopened) = DiskImage::open_auto(backend) {
        let cap = reopened.capacity_bytes();
        if cap > 0 {
            let mut buf = [0u8; 512];
            let _ = reopened.read_at(0, &mut buf[..1]);
            let _ = reopened.read_at(cap - 1, &mut buf[..1]);
        }
        let _ = reopened.flush();
    }
});

