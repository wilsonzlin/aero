#![no_main]

use aero_storage::{AeroSparseDisk, DiskError, MemBackend, Result, StorageBackend, VirtualDisk};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 1024 * 1024; // 1 MiB
const MAX_IO_BYTES: usize = 4096; // 4 KiB
const MAX_OPS: usize = 32;
// Keep backend growth limited even if the parsed image claims a large virtual capacity.
const MAX_TOUCHED_CAP_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB
// Hard cap to avoid pathological allocations (e.g. corrupt images claiming gigantic block_size/data_offset).
const MAX_BACKEND_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB

/// Wrapper around `MemBackend` that refuses to grow beyond `MAX_BACKEND_BYTES`.
///
/// This keeps the fuzz target lightweight even when the parsed image header implies huge
/// allocations on write (e.g. via extreme `block_size_bytes`).
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

    // Treat the fuzzer input as the backing store for a sparse image.
    let mut backend = MemBackend::new();
    // Ensure the header read always succeeds so the fuzzer can discover valid-looking
    // headers even when the initial corpus input is very small.
    let _ = backend.set_len(64);
    let _ = backend.write_at(0, data);

    let mut disk = match AeroSparseDisk::open(CappedBackend { inner: backend }) {
        Ok(disk) => disk,
        Err(_) => return,
    };

    let cap = disk.capacity_bytes();
    if cap == 0 {
        return;
    }

    let mut u = Unstructured::new(data);
    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
    let touched_cap = cap.min(MAX_TOUCHED_CAP_BYTES);
    let mut io_buf = [0u8; MAX_IO_BYTES];

    // Deterministic tiny I/O so we exercise the core read/write logic even if the fuzzer chooses
    // 0 randomized ops.
    if touched_cap > 0 {
        let _ = disk.read_at(0, &mut io_buf[..1]);
        io_buf[0] = data.first().copied().unwrap_or(0).wrapping_add(1);
        let _ = disk.write_at(0, &io_buf[..1]);
        let _ = disk.read_at(touched_cap - 1, &mut io_buf[..1]);
    }

    for _ in 0..ops {
        let is_write: bool = u.arbitrary().unwrap_or(false);
        let len: usize = u.int_in_range(0usize..=MAX_IO_BYTES).unwrap_or(0);
        if len == 0 {
            continue;
        }

        // Keep offsets in-bounds for the chosen `len`.
        let len_u64 = len as u64;
        if touched_cap < len_u64 {
            continue;
        }
        let max_off = touched_cap - len_u64;
        let raw_off: u64 = u.arbitrary().unwrap_or(0);
        let off = if max_off == u64::MAX {
            raw_off
        } else {
            raw_off % (max_off + 1)
        };

        if is_write {
            for b in &mut io_buf[..len] {
                *b = u.arbitrary().unwrap_or(0u8);
            }
            let _ = disk.write_at(off, &io_buf[..len]);
        } else {
            let _ = disk.read_at(off, &mut io_buf[..len]);
        }
    }

    let _ = disk.flush();

    // Re-open after mutation to exercise parsing of updated header/table state.
    let backend = disk.into_backend();
    if let Ok(mut reopened) = AeroSparseDisk::open(backend) {
        let cap = reopened.capacity_bytes();
        if cap > 0 {
            let mut u = Unstructured::new(data);
            let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
            let touched_cap = cap.min(MAX_TOUCHED_CAP_BYTES);
            let mut io_buf = [0u8; MAX_IO_BYTES];

            if touched_cap > 0 {
                let _ = reopened.read_at(0, &mut io_buf[..1]);
                io_buf[0] = data.first().copied().unwrap_or(0).wrapping_add(2);
                let _ = reopened.write_at(0, &io_buf[..1]);
                let _ = reopened.read_at(touched_cap - 1, &mut io_buf[..1]);
            }

            for _ in 0..ops {
                let is_write: bool = u.arbitrary().unwrap_or(false);
                let len: usize = u.int_in_range(0usize..=MAX_IO_BYTES).unwrap_or(0);
                if len == 0 {
                    continue;
                }

                let len_u64 = len as u64;
                if touched_cap < len_u64 {
                    continue;
                }
                let max_off = touched_cap - len_u64;
                let raw_off: u64 = u.arbitrary().unwrap_or(0);
                let off = if max_off == u64::MAX {
                    raw_off
                } else {
                    raw_off % (max_off + 1)
                };

                if is_write {
                    for b in &mut io_buf[..len] {
                        *b = u.arbitrary().unwrap_or(0u8);
                    }
                    let _ = reopened.write_at(off, &io_buf[..len]);
                } else {
                    let _ = reopened.read_at(off, &mut io_buf[..len]);
                }
            }
        }

        let _ = reopened.flush();
    }
});
