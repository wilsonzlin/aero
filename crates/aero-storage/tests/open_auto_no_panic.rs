#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{DiskImage, MemBackend, VirtualDisk};
use proptest::prelude::*;

const MAX_INPUT_BYTES: usize = 256 * 1024;
const MAX_READ_LEN: usize = 4096;
const READS_PER_CASE: usize = 4;

fn bytes_strategy() -> impl Strategy<Value = Vec<u8>> {
    // Bias strongly towards small inputs while still occasionally exercising larger buffers.
    prop_oneof![
        8 => prop::collection::vec(any::<u8>(), 0..=4096),
        4 => prop::collection::vec(any::<u8>(), 0..=(64 * 1024)),
        1 => prop::collection::vec(any::<u8>(), 0..=MAX_INPUT_BYTES),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    #[test]
    fn open_auto_from_untrusted_bytes_does_not_panic(
        data in bytes_strategy(),
        read_seeds in prop::collection::vec(any::<u64>(), READS_PER_CASE),
    ) {
        let backend = MemBackend::from_vec(data);
        let Ok(mut disk) = DiskImage::open_auto(backend) else {
            // Structured errors are fine; we only care about panics.
            return Ok(());
        };

        let capacity = disk.capacity_bytes();
        if capacity == 0 {
            // Still exercise the "empty read" path, which should be a no-op.
            let _ = disk.read_at(0, &mut []);
            return Ok(());
        }

        for seed in read_seeds {
            // Pick a small read length based on the seed, then clamp to disk capacity.
            let mut len_u64 = (seed & 0xFFF) + 1; // 1..=4096
            len_u64 = len_u64.min(MAX_READ_LEN as u64);
            len_u64 = len_u64.min(capacity);

            // `len_u64 <= MAX_READ_LEN`, so this cast is safe on all platforms.
            let len: usize = len_u64 as usize;
            if len == 0 {
                continue;
            }

            let max_offset = capacity - len_u64;
            let offset = seed % (max_offset + 1);

            let mut buf = vec![0u8; len];
            let _ = disk.read_at(offset, &mut buf);
        }
    }
}
