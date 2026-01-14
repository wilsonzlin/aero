#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{AeroSparseConfig, AeroSparseDisk, DiskImage, MemBackend, VirtualDisk};
use proptest::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

const MAX_INPUT_BYTES: usize = 256 * 1024;
const MAX_READ_LEN: usize = 4096;
const READS_PER_CASE: usize = 4;

const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";
const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn vhd_footer_checksum(raw: &[u8; 512]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in raw.iter().enumerate() {
        if (64..68).contains(&i) {
            continue;
        }
        sum = sum.wrapping_add(*b as u32);
    }
    !sum
}

fn vhd_dynamic_header_checksum(raw: &[u8; 1024]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in raw.iter().enumerate() {
        if (36..40).contains(&i) {
            continue;
        }
        sum = sum.wrapping_add(*b as u32);
    }
    !sum
}

fn make_vhd_footer(virtual_size: u64, disk_type: u32, data_offset: u64) -> [u8; 512] {
    let mut footer = [0u8; 512];
    footer[0..8].copy_from_slice(b"conectix");
    write_be_u32(&mut footer, 8, 2); // features
    write_be_u32(&mut footer, 12, 0x0001_0000); // file_format_version
    write_be_u64(&mut footer, 16, data_offset);
    write_be_u64(&mut footer, 40, virtual_size); // original_size
    write_be_u64(&mut footer, 48, virtual_size); // current_size
    write_be_u32(&mut footer, 60, disk_type);
    let checksum = vhd_footer_checksum(&footer);
    write_be_u32(&mut footer, 64, checksum);
    footer
}

fn base_vhd_fixed() -> Vec<u8> {
    let virtual_size: u64 = 64 * 1024;
    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let file_len = (virtual_size + 512) as usize;
    let mut out = vec![0u8; file_len];
    out[virtual_size as usize..virtual_size as usize + 512].copy_from_slice(&footer);
    out
}

fn base_vhd_dynamic() -> Vec<u8> {
    let virtual_size: u64 = 64 * 1024;
    let block_size: u32 = 4096;

    let dyn_header_offset = 512u64;
    let table_offset = dyn_header_offset + 1024;

    let blocks = virtual_size.div_ceil(block_size as u64);
    let max_table_entries = blocks as u32;
    let bat_bytes = max_table_entries as u64 * 4;
    let bat_size = bat_bytes.div_ceil(512) * 512;

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    let file_len = (512 + 1024 + bat_size + 512) as usize;
    let mut out = vec![0u8; file_len];

    // Footer copy at 0 and footer at EOF.
    out[..512].copy_from_slice(&footer);
    out[file_len - 512..].copy_from_slice(&footer);

    // Dynamic header.
    let mut dyn_header = [0u8; 1024];
    dyn_header[0..8].copy_from_slice(b"cxsparse");
    write_be_u64(&mut dyn_header, 8, u64::MAX);
    write_be_u64(&mut dyn_header, 16, table_offset);
    write_be_u32(&mut dyn_header, 24, 0x0001_0000);
    write_be_u32(&mut dyn_header, 28, max_table_entries);
    write_be_u32(&mut dyn_header, 32, block_size);
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    write_be_u32(&mut dyn_header, 36, checksum);
    out[dyn_header_offset as usize..dyn_header_offset as usize + 1024].copy_from_slice(&dyn_header);

    // BAT: all entries unallocated (0xFFFF_FFFF big-endian).
    for b in &mut out[table_offset as usize..table_offset as usize + bat_size as usize] {
        *b = 0xFF;
    }

    out
}

fn base_qcow2_empty() -> Vec<u8> {
    // Keep the image small enough for this test's input-size cap while still exercising
    // the full QCOW2 open path (header + L1 + refcount table + refcount block + L2 table).
    let virtual_size: u64 = 1024 * 1024;
    let cluster_bits = 14u32;
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;

    let file_len = cluster_size * 5;
    let mut out = vec![0u8; file_len as usize];

    // v3 header (104 bytes).
    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(&QCOW2_MAGIC);
    write_be_u32(&mut header, 4, 3); // version
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, virtual_size);
    write_be_u32(&mut header, 36, 1); // l1_size
    write_be_u64(&mut header, 40, l1_table_offset);
    write_be_u64(&mut header, 48, refcount_table_offset);
    write_be_u32(&mut header, 56, 1); // refcount_table_clusters
    write_be_u64(&mut header, 72, 0); // incompatible_features
    write_be_u32(&mut header, 96, 4); // refcount_order
    write_be_u32(&mut header, 100, 104); // header_length
    out[..104].copy_from_slice(&header);

    // Refcount table: first entry points at the refcount block cluster.
    out[refcount_table_offset as usize..refcount_table_offset as usize + 8]
        .copy_from_slice(&refcount_block_offset.to_be_bytes());

    // L1 table: single entry points at the L2 table cluster.
    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    out[l1_table_offset as usize..l1_table_offset as usize + 8].copy_from_slice(&l1_entry.to_be_bytes());

    out
}

fn base_aerosparse_empty() -> Vec<u8> {
    let disk = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 64 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    disk.into_backend().into_vec()
}

fn apply_mutations(mut bytes: Vec<u8>, truncate_seed: u32, mutations: &[(u32, u8)]) -> Vec<u8> {
    if !bytes.is_empty() {
        for &(idx, val) in mutations {
            let i = (idx as usize) % bytes.len();
            bytes[i] ^= val;
        }
    }

    // Randomly truncate to exercise format detection + header parsing against truncated inputs.
    //
    // Also keep the full length occasionally so we exercise successful open + read paths for
    // valid/near-valid images, not only the "truncated header/table" errors.
    if !bytes.is_empty() && (truncate_seed & 0xF) != 0 {
        let new_len = (truncate_seed as usize) % (bytes.len() + 1);
        bytes.truncate(new_len);
    }
    bytes
}

fn bytes_strategy() -> impl Strategy<Value = Vec<u8>> {
    // Bias strongly towards small inputs while still occasionally exercising larger buffers.
    //
    // Additionally, bias towards "format-looking" inputs by starting from small valid images and
    // applying random byte-level corruption + truncation. Pure random bytes almost never match the
    // QCOW2/VHD/AeroSparse magic values, so this greatly improves coverage of `open_auto` parsing
    // paths.
    let random = prop_oneof![
        8 => prop::collection::vec(any::<u8>(), 0..=4096),
        4 => prop::collection::vec(any::<u8>(), 0..=(64 * 1024)),
        1 => prop::collection::vec(any::<u8>(), 0..=MAX_INPUT_BYTES),
    ];

    let mutations = prop::collection::vec((any::<u32>(), any::<u8>()), 0..=32);

    let qcow2 = (any::<u32>(), mutations.clone()).prop_map(|(truncate, muts)| {
        apply_mutations(base_qcow2_empty(), truncate, &muts)
    });

    let aerosparse = (any::<u32>(), mutations.clone()).prop_map(|(truncate, muts)| {
        apply_mutations(base_aerosparse_empty(), truncate, &muts)
    });

    let vhd_fixed = (any::<u32>(), mutations.clone()).prop_map(|(truncate, muts)| {
        apply_mutations(base_vhd_fixed(), truncate, &muts)
    });

    let vhd_dynamic = (any::<u32>(), mutations).prop_map(|(truncate, muts)| {
        apply_mutations(base_vhd_dynamic(), truncate, &muts)
    });

    prop_oneof![
        6 => random,
        2 => qcow2,
        2 => aerosparse,
        1 => vhd_fixed,
        1 => vhd_dynamic,
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
        let open_res = catch_unwind(AssertUnwindSafe(|| DiskImage::open_auto(backend)));
        let mut disk = match open_res {
            Ok(Ok(disk)) => disk,
            Ok(Err(_)) => {
                // Structured errors are fine; we only care about panics.
                return Ok(());
            }
            Err(_) => {
                prop_assert!(false, "DiskImage::open_auto panicked");
                unreachable!();
            }
        };

        let capacity = disk.capacity_bytes();
        if capacity == 0 {
            // Still exercise the "empty read" path, which should be a no-op.
            let _ = catch_unwind(AssertUnwindSafe(|| disk.read_at(0, &mut [])))
                .map_err(|_| TestCaseError::fail("DiskImage::read_at panicked"))?;
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
            let read_res = catch_unwind(AssertUnwindSafe(|| disk.read_at(offset, &mut buf)));
            prop_assert!(
                read_res.is_ok(),
                "DiskImage::read_at panicked (cap={capacity}, offset={offset}, len={len})"
            );
            let _ = read_res.unwrap();
        }

        let flush_res = catch_unwind(AssertUnwindSafe(|| disk.flush()));
        prop_assert!(flush_res.is_ok(), "DiskImage::flush panicked");
    }
}
