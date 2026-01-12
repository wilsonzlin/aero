#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    AeroCowDisk, AeroSparseConfig, AeroSparseDisk, BlockCachedDisk, MemBackend, RawDisk,
    VirtualDisk,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseResult;

const MAX_CAPACITY_BYTES: u64 = 128 * 1024;
const MAX_OPS_PER_CASE: usize = 50;
const MAX_RW_LEN: usize = 4096;

#[derive(Clone, Debug)]
enum Op {
    Read { offset: u64, len: usize },
    Write { offset: u64, data: Vec<u8> },
    Flush,
}

fn offset_len_strategy(capacity: u64) -> impl Strategy<Value = (u64, usize)> {
    debug_assert!(capacity > 0);

    // Bias towards interesting boundaries while still covering the whole disk.
    let offset = prop_oneof![
        2 => 0u64..capacity,
        1 => Just(0u64),
        1 => Just(capacity - 1),
        1 => Just(capacity / 2),
    ];

    offset.prop_flat_map(move |offset| {
        let remaining = capacity - offset;
        let max_len = (remaining.min(MAX_RW_LEN as u64)) as usize;
        debug_assert!(max_len > 0);

        prop_oneof![
            1 => Just(1usize),
            1 => Just(max_len),
            2 => 1usize..=max_len,
        ]
        .prop_map(move |len| (offset, len))
    })
}

fn op_strategy(capacity: u64) -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => offset_len_strategy(capacity).prop_map(|(offset, len)| Op::Read { offset, len }),
        4 => offset_len_strategy(capacity).prop_flat_map(|(offset, len)| {
            prop::collection::vec(any::<u8>(), len)
                .prop_map(move |data| Op::Write { offset, data })
        }),
        1 => Just(Op::Flush),
    ]
}

fn disk_case_strategy(
    capacity_range: impl Strategy<Value = u64>,
) -> impl Strategy<Value = (u64, Vec<Op>)> {
    capacity_range.prop_flat_map(|capacity| {
        let ops = prop::collection::vec(op_strategy(capacity), 1..=MAX_OPS_PER_CASE);
        (Just(capacity), ops)
    })
}

fn disk_case_with_reads_strategy(
    capacity_range: impl Strategy<Value = u64>,
) -> impl Strategy<Value = (u64, Vec<Op>, Vec<(u64, usize)>)> {
    capacity_range.prop_flat_map(|capacity| {
        let ops = prop::collection::vec(op_strategy(capacity), 1..=MAX_OPS_PER_CASE);
        let reads = prop::collection::vec(offset_len_strategy(capacity), 1..=8);
        (Just(capacity), ops, reads)
    })
}

fn apply_ops<D: VirtualDisk>(disk: &mut D, model: &mut [u8], ops: &[Op]) -> TestCaseResult {
    for op in ops {
        match op {
            Op::Read { offset, len } => {
                let offset_usize: usize = (*offset).try_into().unwrap();
                let end = offset_usize + *len;
                let mut buf = vec![0u8; *len];
                disk.read_at(*offset, &mut buf)
                    .map_err(|e| TestCaseError::fail(format!("read_at failed: {e:?}")))?;
                prop_assert_eq!(buf.as_slice(), &model[offset_usize..end]);
            }
            Op::Write { offset, data } => {
                let offset_usize: usize = (*offset).try_into().unwrap();
                let end = offset_usize + data.len();
                disk.write_at(*offset, data)
                    .map_err(|e| TestCaseError::fail(format!("write_at failed: {e:?}")))?;
                model[offset_usize..end].copy_from_slice(data);
            }
            Op::Flush => {
                disk.flush()
                    .map_err(|e| TestCaseError::fail(format!("flush failed: {e:?}")))?;
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    #[test]
    fn raw_disk_matches_reference((capacity, ops) in disk_case_strategy(1u64..=MAX_CAPACITY_BYTES)) {
        let capacity_usize: usize = capacity.try_into().unwrap();
        let mut model = vec![0u8; capacity_usize];

        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        prop_assert_eq!(disk.capacity_bytes(), capacity);

        apply_ops(&mut disk, &mut model, &ops)?;

        disk.flush().unwrap();
        let backend = disk.into_backend();
        prop_assert_eq!(backend.as_slice(), model.as_slice());
    }

    #[test]
    fn sparse_disk_matches_reference_and_persists(
        (capacity, ops, reads) in disk_case_with_reads_strategy(
            // `AeroSparseDisk` represents a sector-backed disk image; require a 512-byte
            // aligned capacity so the created image can always be reopened.
            (1u64..=(MAX_CAPACITY_BYTES / 512)).prop_map(|sectors| sectors * 512),
        )
    ) {
        let capacity_usize: usize = capacity.try_into().unwrap();
        let mut model = vec![0u8; capacity_usize];

        let mut disk = AeroSparseDisk::create(
            MemBackend::new(),
            AeroSparseConfig {
                disk_size_bytes: capacity,
                block_size_bytes: 4096,
            },
        ).unwrap();

        // Unallocated regions must read as zero.
        let mut initial = vec![0xAAu8; (capacity_usize).min(1024)];
        disk.read_at(0, &mut initial).unwrap();
        prop_assert!(initial.iter().all(|&b| b == 0));

        apply_ops(&mut disk, &mut model, &ops)?;

        disk.flush().unwrap();
        let backend = disk.into_backend();

        let mut reopened = AeroSparseDisk::open(backend).unwrap();
        prop_assert_eq!(reopened.capacity_bytes(), capacity);

        for (offset, len) in reads {
            let offset_usize: usize = offset.try_into().unwrap();
            let end = offset_usize + len;
            let mut buf = vec![0u8; len];
            reopened.read_at(offset, &mut buf).unwrap();
            prop_assert_eq!(buf.as_slice(), &model[offset_usize..end]);
        }
    }

    #[test]
    fn cow_disk_matches_reference_base_plus_overlay((capacity, base_data, ops) in (1u64..=(MAX_CAPACITY_BYTES / 512))
        .prop_map(|sectors| sectors * 512)
        .prop_flat_map(|capacity| {
        let cap_usize: usize = capacity.try_into().unwrap();
        (
            Just(capacity),
            prop::collection::vec(any::<u8>(), cap_usize),
            prop::collection::vec(op_strategy(capacity), 1..=MAX_OPS_PER_CASE),
        )
    })) {
        let capacity_usize: usize = capacity.try_into().unwrap();
        let base_initial = base_data.clone();
        let mut model = base_data;

        let mut base = RawDisk::create(MemBackend::new(), capacity).unwrap();
        base.write_at(0, &model).unwrap();

        let mut cow = AeroCowDisk::create(base, MemBackend::new(), 4096).unwrap();

        // Before any writes, reads must come from the base disk.
        if capacity_usize > 0 {
            let len = capacity_usize.min(1024);
            let mut buf = vec![0u8; len];
            cow.read_at(0, &mut buf).unwrap();
            prop_assert_eq!(buf.as_slice(), &model[..len]);
        }

        apply_ops(&mut cow, &mut model, &ops)?;
        cow.flush().unwrap();

        let (base, _overlay) = cow.into_parts();
        let base_backend = base.into_backend();
        prop_assert_eq!(base_backend.as_slice(), base_initial.as_slice());
    }

    #[test]
    fn block_cached_disk_matches_reference_and_writes_back((capacity, ops) in disk_case_strategy((3u64 * 1024)..=MAX_CAPACITY_BYTES)) {
        const BLOCK_SIZE: usize = 1024;
        const MAX_CACHED_BLOCKS: usize = 2;

        let capacity_usize: usize = capacity.try_into().unwrap();
        let mut model = vec![0u8; capacity_usize];

        let raw = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut cached = BlockCachedDisk::new(raw, BLOCK_SIZE, MAX_CACHED_BLOCKS).unwrap();

        apply_ops(&mut cached, &mut model, &ops)?;

        // Force at least one eviction of a dirty block (exercise write-back-on-evict).
        for (block, pattern) in [(0u64, 0xA1u8), (1u64, 0xB2u8), (2u64, 0xC3u8)] {
            let offset = block * BLOCK_SIZE as u64;
            let len = 32usize.min(capacity_usize.saturating_sub(offset as usize));
            if len == 0 {
                continue;
            }
            let data = vec![pattern; len];
            cached.write_at(offset, &data).unwrap();
            let off = offset as usize;
            model[off..off + len].copy_from_slice(&data);
        }

        cached.flush().unwrap();
        let raw = cached.into_inner();
        let backend = raw.into_backend();
        prop_assert_eq!(backend.as_slice(), model.as_slice());
    }
}
