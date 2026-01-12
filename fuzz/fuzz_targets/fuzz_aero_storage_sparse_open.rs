#![no_main]

use aero_storage::{AeroSparseDisk, DiskError, Result, StorageBackend, VirtualDisk};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 1024 * 1024; // 1 MiB
const MAX_IO_BYTES: usize = 4096; // 4 KiB
const MAX_OPS: usize = 32;
// Keep backend growth limited even if the parsed image claims a large virtual capacity.
const MAX_TOUCHED_CAP_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB
// Keep the in-memory backing store from growing too large (even if block_size/data_offset are huge).
const MAX_BACKEND_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

/// In-memory backend with a hard cap on growth.
///
/// This keeps the fuzz target lightweight even if the input claims absurd sparse parameters.
#[derive(Default)]
struct CappedBackend {
    data: Vec<u8>,
}

impl StorageBackend for CappedBackend {
    fn len(&mut self) -> Result<u64> {
        Ok(self.data.len() as u64)
    }

    fn set_len(&mut self, len: u64) -> Result<()> {
        if len > MAX_BACKEND_BYTES as u64 {
            return Err(DiskError::QuotaExceeded);
        }
        let len_usize: usize = len.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        self.data.resize(len_usize, 0);
        Ok(())
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

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let offset_usize: usize = offset.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset_usize
            .checked_add(buf.len())
            .ok_or(DiskError::OffsetOverflow)?;
        if end > MAX_BACKEND_BYTES {
            return Err(DiskError::QuotaExceeded);
        }
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[offset_usize..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Treat the fuzzer input as the backing store for a sparse image.
    let mut backend = CappedBackend::default();
    // Ensure the header read always succeeds so the fuzzer can discover valid-looking
    // headers even when the initial corpus input is very small.
    let _ = backend.set_len(64);
    let _ = backend.write_at(0, data);

    let mut disk = match AeroSparseDisk::open(backend) {
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
