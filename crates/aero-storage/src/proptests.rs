use crate::{
    AeroCowDisk, AeroSparseConfig, AeroSparseDisk, BlockCachedDisk, MemBackend, RawDisk,
    VirtualDisk, SECTOR_SIZE,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseResult;

#[derive(Debug, Clone)]
enum Op {
    Write { offset: u32, data: Vec<u8> },
    Read { offset: u32, len: usize },
    Flush,
    Reopen,
}

const MAX_DISK_SIZE: u32 = 1024 * 1024; // 1 MiB
const MAX_OPS: usize = 64;
const MAX_RW_LEN: usize = 8 * 1024;

const SECTOR_SIZE_U32: u32 = SECTOR_SIZE as u32;
const MAX_DISK_SECTORS: u32 = MAX_DISK_SIZE / SECTOR_SIZE_U32;

fn disk_size_strategy() -> impl Strategy<Value = u32> {
    // Many disk layers assume a sector-addressable device.
    (1u32..=MAX_DISK_SECTORS).prop_map(|sectors| sectors * SECTOR_SIZE_U32)
}

fn div_ceil_u32(n: u32, d: u32) -> u32 {
    (n + d - 1) / d
}

fn sparse_block_size_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(512u32),
        Just(1024u32),
        Just(4096u32),
        Just(16 * 1024u32),
    ]
}

fn cache_block_size_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![Just(512usize), Just(1024usize), Just(3072usize), Just(4096usize)]
}

fn max_cached_blocks_strategy() -> impl Strategy<Value = usize> {
    // Small cache sizes ensure we hit eviction/writeback paths frequently.
    prop_oneof![Just(1usize), Just(2usize)]
}

fn offset_strategy(disk_size: u32) -> BoxedStrategy<u32> {
    let max_offset = disk_size;

    let any = 0u32..=max_offset;
    let sector_aligned =
        (0u32..=max_offset / SECTOR_SIZE_U32).prop_map(|lba| lba * SECTOR_SIZE_U32);
    let sector_boundary_plus_delta = (0u32..=max_offset / SECTOR_SIZE_U32, 0u32..SECTOR_SIZE_U32)
        .prop_map(move |(lba, delta)| {
            let off = lba * SECTOR_SIZE_U32 + delta;
            off.min(max_offset)
        });
    let block_aligned = (0u32..=max_offset / 4096).prop_map(|blk| blk * 4096);
    let near_end =
        (0u32..=SECTOR_SIZE_U32).prop_map(move |delta| max_offset.saturating_sub(delta));

    prop_oneof![
        4 => any,
        2 => sector_aligned,
        2 => sector_boundary_plus_delta,
        2 => block_aligned,
        1 => near_end,
    ]
    .boxed()
}

fn write_op_strategy(disk_size: u32) -> BoxedStrategy<Op> {
    offset_strategy(disk_size)
        .prop_flat_map(move |offset| {
            let remaining = disk_size - offset;
            let max_len = (remaining as usize).min(MAX_RW_LEN);
            (Just(offset), prop::collection::vec(any::<u8>(), 0..=max_len))
        })
        .prop_map(|(offset, data)| Op::Write { offset, data })
        .boxed()
}

fn read_op_strategy(disk_size: u32) -> BoxedStrategy<Op> {
    offset_strategy(disk_size)
        .prop_flat_map(move |offset| {
            let remaining = disk_size - offset;
            let max_len = (remaining as usize).min(MAX_RW_LEN);
            (Just(offset), 0usize..=max_len)
        })
        .prop_map(|(offset, len)| Op::Read { offset, len })
        .boxed()
}

fn op_strategy(disk_size: u32) -> BoxedStrategy<Op> {
    prop_oneof![
        5 => write_op_strategy(disk_size),
        4 => read_op_strategy(disk_size),
        1 => Just(Op::Flush),
        1 => Just(Op::Reopen),
    ]
    .boxed()
}

fn ops_strategy(disk_size: u32) -> BoxedStrategy<Vec<Op>> {
    prop::collection::vec(op_strategy(disk_size), 1..=MAX_OPS).boxed()
}

fn raw_scenario_strategy() -> BoxedStrategy<(u32, Vec<Op>)> {
    disk_size_strategy()
        .prop_flat_map(|disk_size| (Just(disk_size), ops_strategy(disk_size)))
        .boxed()
}

fn sparse_scenario_strategy() -> BoxedStrategy<(u32, u32, Vec<Op>)> {
    (disk_size_strategy(), sparse_block_size_strategy())
        .prop_flat_map(|(disk_size, block_size)| {
            (Just(disk_size), Just(block_size), ops_strategy(disk_size))
        })
        .boxed()
}

fn cow_scenario_strategy() -> BoxedStrategy<(u32, u32, u8, Vec<Op>)> {
    (disk_size_strategy(), sparse_block_size_strategy(), any::<u8>())
        .prop_flat_map(|(disk_size, block_size, seed)| {
            (Just(disk_size), Just(block_size), Just(seed), ops_strategy(disk_size))
        })
        .boxed()
}

fn cached_raw_scenario_strategy() -> BoxedStrategy<(u32, usize, usize, Vec<Op>)> {
    cache_block_size_strategy()
        .prop_flat_map(|cache_block_size| {
            let min_disk_size = (cache_block_size * 3) as u32;
            (
                div_ceil_u32(min_disk_size, SECTOR_SIZE_U32)..=MAX_DISK_SECTORS,
                Just(cache_block_size),
                max_cached_blocks_strategy(),
            )
        })
        .prop_flat_map(|(disk_sectors, cache_block_size, max_cached_blocks)| {
            let disk_size = disk_sectors * SECTOR_SIZE_U32;
            (
                Just(disk_size),
                Just(cache_block_size),
                Just(max_cached_blocks),
                ops_strategy(disk_size),
            )
        })
        .boxed()
}

fn cached_sparse_scenario_strategy() -> BoxedStrategy<(u32, u32, usize, usize, Vec<Op>)> {
    (sparse_block_size_strategy(), cache_block_size_strategy())
        .prop_flat_map(|(sparse_block_size, cache_block_size)| {
            let min_disk_size = (cache_block_size * 3) as u32;
            (
                div_ceil_u32(min_disk_size, SECTOR_SIZE_U32)..=MAX_DISK_SECTORS,
                Just(sparse_block_size),
                Just(cache_block_size),
                max_cached_blocks_strategy(),
            )
        })
        .prop_flat_map(|(disk_sectors, sparse_block_size, cache_block_size, max_cached_blocks)| {
            let disk_size = disk_sectors * SECTOR_SIZE_U32;
            (
                Just(disk_size),
                Just(sparse_block_size),
                Just(cache_block_size),
                Just(max_cached_blocks),
                ops_strategy(disk_size),
            )
        })
        .boxed()
}

fn cached_cow_scenario_strategy() -> BoxedStrategy<(u32, u32, u8, usize, usize, Vec<Op>)> {
    (sparse_block_size_strategy(), cache_block_size_strategy(), any::<u8>())
        .prop_flat_map(|(cow_block_size, cache_block_size, seed)| {
            let min_disk_size = (cache_block_size * 3) as u32;
            (
                div_ceil_u32(min_disk_size, SECTOR_SIZE_U32)..=MAX_DISK_SECTORS,
                Just(cow_block_size),
                Just(seed),
                Just(cache_block_size),
                max_cached_blocks_strategy(),
            )
        })
        .prop_flat_map(
            |(disk_sectors, cow_block_size, seed, cache_block_size, max_cached_blocks)| {
                let disk_size = disk_sectors * SECTOR_SIZE_U32;
                (
                    Just(disk_size),
                    Just(cow_block_size),
                    Just(seed),
                    Just(cache_block_size),
                    Just(max_cached_blocks),
                    ops_strategy(disk_size),
                )
            },
        )
        .boxed()
}

fn run_ops<D, Reopen>(
    mut disk: D,
    mut model: Vec<u8>,
    ops: &[Op],
    mut reopen: Reopen,
) -> TestCaseResult
where
    D: VirtualDisk,
    Reopen: FnMut(D) -> D,
{
    let capacity = disk.capacity_bytes() as usize;
    prop_assert_eq!(capacity, model.len());

    for op in ops {
        match op {
            Op::Write { offset, data } => {
                let offset = *offset as usize;
                disk.write_at(offset as u64, data).unwrap();
                model[offset..offset + data.len()].copy_from_slice(data);

                // Read-after-write must match what we wrote.
                let mut read_back = vec![0xA5u8; data.len()];
                disk.read_at(offset as u64, &mut read_back).unwrap();
                prop_assert_eq!(read_back.as_slice(), data.as_slice());
            }
            Op::Read { offset, len } => {
                let offset = *offset as usize;
                let len = *len;
                let mut buf = vec![0xA5u8; len];
                disk.read_at(offset as u64, &mut buf).unwrap();
                prop_assert_eq!(buf.as_slice(), &model[offset..offset + len]);
            }
            Op::Flush => {
                disk.flush().unwrap();
            }
            Op::Reopen => {
                // Model "close" as a flush + re-open using the same backend.
                disk.flush().unwrap();
                disk = reopen(disk);
                prop_assert_eq!(disk.capacity_bytes() as usize, capacity);
            }
        }
    }

    // Ensure persisted correctness across a final close/open cycle.
    disk.flush().unwrap();
    disk = reopen(disk);
    prop_assert_eq!(disk.capacity_bytes() as usize, capacity);

    let mut all = vec![0u8; capacity];
    disk.read_at(0, &mut all).unwrap();
    prop_assert_eq!(all.as_slice(), model.as_slice());

    Ok(())
}

fn make_base_pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u32).wrapping_mul(31).wrapping_add(seed as u32) as u8)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_raw_disk_matches_reference((disk_size, ops) in raw_scenario_strategy()) {
        let disk = RawDisk::create(MemBackend::new(), disk_size as u64).unwrap();
        let model = vec![0u8; disk_size as usize];

        run_ops(disk, model, &ops, |disk| RawDisk::open(disk.into_backend()).unwrap())?;
    }

    #[test]
    fn prop_sparse_disk_matches_reference((disk_size, block_size, ops) in sparse_scenario_strategy()) {
        let disk = AeroSparseDisk::create(
            MemBackend::new(),
            AeroSparseConfig {
                disk_size_bytes: disk_size as u64,
                block_size_bytes: block_size,
            },
        )
        .unwrap();
        let model = vec![0u8; disk_size as usize];

        run_ops(disk, model, &ops, |disk| AeroSparseDisk::open(disk.into_backend()).unwrap())?;
    }

    #[test]
    fn prop_cow_disk_matches_reference((disk_size, block_size, seed, ops) in cow_scenario_strategy()) {
        let mut base = RawDisk::create(MemBackend::new(), disk_size as u64).unwrap();
        let model = make_base_pattern(disk_size as usize, seed);
        base.write_at(0, &model).unwrap();

        let disk = AeroCowDisk::create(base, MemBackend::new(), block_size).unwrap();

        run_ops(disk, model, &ops, |disk| {
            let (base, overlay) = disk.into_parts();
            let base = RawDisk::open(base.into_backend()).unwrap();
            AeroCowDisk::open(base, overlay.into_backend()).unwrap()
        })?;
    }

    #[test]
    fn prop_cached_raw_disk_matches_reference((disk_size, cache_block_size, max_cached_blocks, ops) in cached_raw_scenario_strategy()) {
        let raw = RawDisk::create(MemBackend::new(), disk_size as u64).unwrap();
        let disk = BlockCachedDisk::new(raw, cache_block_size, max_cached_blocks).unwrap();
        let model = vec![0u8; disk_size as usize];

        run_ops(disk, model, &ops, move |disk| {
            let inner = disk.into_inner();
            let inner = RawDisk::open(inner.into_backend()).unwrap();
            BlockCachedDisk::new(inner, cache_block_size, max_cached_blocks).unwrap()
        })?;
    }

    #[test]
    fn prop_cached_sparse_disk_matches_reference((disk_size, sparse_block_size, cache_block_size, max_cached_blocks, ops) in cached_sparse_scenario_strategy()) {
        let inner = AeroSparseDisk::create(
            MemBackend::new(),
            AeroSparseConfig {
                disk_size_bytes: disk_size as u64,
                block_size_bytes: sparse_block_size,
            },
        )
        .unwrap();
        let disk = BlockCachedDisk::new(inner, cache_block_size, max_cached_blocks).unwrap();
        let model = vec![0u8; disk_size as usize];

        run_ops(disk, model, &ops, move |disk| {
            let inner = disk.into_inner();
            let inner = AeroSparseDisk::open(inner.into_backend()).unwrap();
            BlockCachedDisk::new(inner, cache_block_size, max_cached_blocks).unwrap()
        })?;
    }

    #[test]
    fn prop_cached_cow_disk_matches_reference((disk_size, cow_block_size, seed, cache_block_size, max_cached_blocks, ops) in cached_cow_scenario_strategy()) {
        let mut base = RawDisk::create(MemBackend::new(), disk_size as u64).unwrap();
        let model = make_base_pattern(disk_size as usize, seed);
        base.write_at(0, &model).unwrap();
        let cow = AeroCowDisk::create(base, MemBackend::new(), cow_block_size).unwrap();

        let disk = BlockCachedDisk::new(cow, cache_block_size, max_cached_blocks).unwrap();

        run_ops(disk, model, &ops, move |disk| {
            let cow = disk.into_inner();
            let (base, overlay) = cow.into_parts();
            let base = RawDisk::open(base.into_backend()).unwrap();
            let cow = AeroCowDisk::open(base, overlay.into_backend()).unwrap();
            BlockCachedDisk::new(cow, cache_block_size, max_cached_blocks).unwrap()
        })?;
    }
}
