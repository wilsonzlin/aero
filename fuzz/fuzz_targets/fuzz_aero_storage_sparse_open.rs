#![no_main]

use aero_storage::{AeroSparseDisk, MemBackend, StorageBackend, VirtualDisk};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 1024 * 1024; // 1 MiB
const MAX_IO_BYTES: usize = 4096; // 4 KiB
const MAX_OPS: usize = 32;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Treat the fuzzer input as the backing store for a sparse image.
    let mut backend = MemBackend::new();
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
    for _ in 0..ops {
        let is_write: bool = u.arbitrary().unwrap_or(false);
        let len: usize = u.int_in_range(0usize..=MAX_IO_BYTES).unwrap_or(0);
        if len == 0 {
            continue;
        }

        // Keep offsets in-bounds for the chosen `len`.
        let len_u64 = len as u64;
        if cap < len_u64 {
            continue;
        }
        let max_off = cap - len_u64;
        let raw_off: u64 = u.arbitrary().unwrap_or(0);
        let off = if max_off == u64::MAX {
            raw_off
        } else {
            raw_off % (max_off + 1)
        };

        if is_write {
            let mut buf = vec![0u8; len];
            for b in &mut buf {
                *b = u.arbitrary().unwrap_or(0);
            }
            let _ = disk.write_at(off, &buf);
        } else {
            let mut buf = vec![0u8; len];
            let _ = disk.read_at(off, &mut buf);
        }
    }

    let _ = disk.flush();
});

