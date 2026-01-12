use crate::{
    AeroCowDisk, AeroSparseConfig, AeroSparseDisk, AeroSparseHeader, BlockCachedDisk, DiskError,
    MemBackend, RawDisk, StorageBackend as _, VirtualDisk, SECTOR_SIZE,
};

fn make_header(
    disk_size_bytes: u64,
    block_size_bytes: u32,
    allocated_blocks: u64,
) -> AeroSparseHeader {
    let table_entries = disk_size_bytes.div_ceil(block_size_bytes as u64);
    let table_bytes = table_entries * 8;
    let data_offset = (crate::sparse::HEADER_SIZE as u64 + table_bytes)
        .div_ceil(block_size_bytes as u64)
        * block_size_bytes as u64;
    AeroSparseHeader {
        version: 1,
        block_size_bytes,
        disk_size_bytes,
        table_entries,
        data_offset,
        allocated_blocks,
    }
}

fn write_table(backend: &mut MemBackend, table: &[u64]) {
    let mut table_bytes = Vec::with_capacity(table.len() * 8);
    for &v in table {
        table_bytes.extend_from_slice(&v.to_le_bytes());
    }
    backend
        .write_at(crate::sparse::HEADER_SIZE as u64, &table_bytes)
        .unwrap();
}

fn open_sparse_err(backend: MemBackend) -> DiskError {
    match AeroSparseDisk::open(backend) {
        Ok(_) => panic!("expected open to fail"),
        Err(e) => e,
    }
}

#[test]
fn sector_helpers_validate_alignment_and_bounds() {
    let backend = MemBackend::with_len((SECTOR_SIZE * 8) as u64).unwrap();
    let mut disk = RawDisk::open(backend).unwrap();

    let mut buf = [0u8; 513];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::UnalignedLength { .. }));

    let mut buf = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(9, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::OutOfBounds { .. }));

    // LBA -> byte offset overflow should be surfaced explicitly.
    let err = disk.read_sectors(u64::MAX, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::OffsetOverflow));

    let err = disk.write_sectors(u64::MAX, &buf).unwrap_err();
    assert!(matches!(err, DiskError::OffsetOverflow));
}

#[test]
fn sparse_disk_reads_zero_until_allocated() {
    let backend = MemBackend::new();
    let mut disk = AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();

    let mut buf = vec![0xAAu8; 1024];
    disk.read_at(1234, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn doc_example_open_auto_works() {
    let backend = MemBackend::with_len(1024 * 1024).unwrap();
    let mut disk = crate::DiskImage::open_auto(backend).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
}

#[test]
fn sparse_disk_allocates_and_persists() {
    let backend = MemBackend::new();
    let mut disk = AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();

    disk.write_at(5000, &[1, 2, 3, 4]).unwrap();
    disk.flush().unwrap();
    assert_eq!(disk.header().allocated_blocks, 1);

    let backend = disk.into_backend();
    let mut reopened = AeroSparseDisk::open(backend).unwrap();
    assert_eq!(reopened.header().allocated_blocks, 1);

    let mut buf = [0u8; 4];
    reopened.read_at(5000, &mut buf).unwrap();
    assert_eq!(&buf, &[1, 2, 3, 4]);

    let mut zeros = [0xFFu8; 8];
    reopened.read_at(0, &mut zeros).unwrap();
    assert!(zeros.iter().all(|&b| b == 0));
}

#[test]
fn cow_disk_reads_from_base_and_writes_to_overlay() {
    let mut base = RawDisk::create(MemBackend::new(), 8192).unwrap();
    let pattern: Vec<u8> = (0..base.capacity_bytes() as usize)
        .map(|i| (i & 0xFF) as u8)
        .collect();
    base.write_at(0, &pattern).unwrap();

    let mut cow = AeroCowDisk::create(base, MemBackend::new(), 4096).unwrap();

    let mut buf = vec![0u8; 64];
    cow.read_at(100, &mut buf).unwrap();
    assert_eq!(&buf, &pattern[100..164]);

    cow.write_at(120, &[9, 9, 9, 9]).unwrap();
    cow.flush().unwrap();
    assert_eq!(cow.overlay().header().allocated_blocks, 1);

    cow.read_at(116, &mut buf[..16]).unwrap();
    assert_eq!(&buf[..4], &pattern[116..120]);
    assert_eq!(&buf[4..8], &[9, 9, 9, 9]);
    assert_eq!(&buf[8..16], &pattern[124..132]);
}

#[test]
fn block_cache_eviction_writes_back_dirty_blocks() {
    let raw = RawDisk::create(MemBackend::new(), 48).unwrap();
    let mut cached = BlockCachedDisk::new(raw, 16, 2).unwrap();

    cached.write_at(0, &[1, 2, 3, 4]).unwrap(); // block 0
    cached.write_at(16, &[5, 6, 7, 8]).unwrap(); // block 1
    cached.write_at(32, &[9, 10, 11, 12]).unwrap(); // block 2 -> evicts block 0

    let mut buf = [0u8; 4];
    cached.inner_mut().read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, &[1, 2, 3, 4]);

    cached.flush().unwrap();
    let stats = cached.stats();
    assert!(stats.evictions >= 1);
    assert!(stats.writebacks >= 1);
}

#[test]
fn block_cache_reports_allocation_failure_as_quota_exceeded() {
    // Use an absurd block size that should fail `try_reserve_exact` deterministically (capacity
    // overflow) without actually attempting to allocate.
    let raw = RawDisk::create(MemBackend::new(), 512).unwrap();
    let mut cached = BlockCachedDisk::new(raw, usize::MAX, 1).unwrap();

    let mut buf = [0u8; 1];
    let err = cached.read_at(0, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::QuotaExceeded));
}

#[test]
fn boxed_virtual_disk_can_be_used_in_generic_wrappers() {
    let raw = RawDisk::create(MemBackend::new(), (SECTOR_SIZE * 8) as u64).unwrap();
    let boxed: Box<dyn VirtualDisk> = Box::new(raw);

    // Compile-time check: `Box<dyn VirtualDisk>` itself implements `VirtualDisk` so it can be used
    // in generic wrappers like `BlockCachedDisk`.
    let mut cached = BlockCachedDisk::new(boxed, SECTOR_SIZE, 1).unwrap();

    cached.write_at(0, &[1, 2, 3, 4]).unwrap();
    cached.flush().unwrap();

    let mut buf = [0u8; 4];
    cached.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, &[1, 2, 3, 4]);
}

#[test]
fn sparse_open_rejects_oversized_allocation_table() {
    // Craft a header that claims an allocation table larger than the hard cap.
    // The backend only contains the header; `open` must reject the image without allocating the
    // claimed table size.
    let block_size_bytes = 4096u32;
    let table_entries = (128 * 1024 * 1024 / 8) + 1;
    let disk_size_bytes = table_entries * (block_size_bytes as u64);
    let table_bytes = table_entries * 8;
    let data_offset = crate::util::align_up_u64(
        (crate::sparse::HEADER_SIZE as u64) + table_bytes,
        block_size_bytes as u64,
    )
    .unwrap();

    let header = AeroSparseHeader {
        version: 1,
        block_size_bytes,
        disk_size_bytes,
        table_entries,
        data_offset,
        allocated_blocks: 0,
    };

    let mut backend = MemBackend::new();
    backend.write_at(0, &header.encode()).unwrap();

    let err = open_sparse_err(backend);
    assert!(matches!(
        err,
        DiskError::Unsupported(_) | DiskError::InvalidSparseHeader(_)
    ));
}

#[test]
fn sparse_open_rejects_allocated_blocks_inconsistent_with_table() {
    // Valid header/table layout, but header.allocated_blocks does not match the number
    // of non-zero table entries.
    let block_size_bytes = 4096u32;
    let disk_size_bytes = 16 * 1024u64;
    let table_entries = 4u64;
    let table_bytes = table_entries * 8;
    let data_offset = crate::util::align_up_u64(
        (crate::sparse::HEADER_SIZE as u64) + table_bytes,
        block_size_bytes as u64,
    )
    .unwrap();

    let header = AeroSparseHeader {
        version: 1,
        block_size_bytes,
        disk_size_bytes,
        table_entries,
        data_offset,
        allocated_blocks: 2, // but we will only store one non-zero entry
    };

    // Create a backend large enough to satisfy the header's claimed allocated block count.
    let mut backend = MemBackend::with_len(data_offset + 2 * block_size_bytes as u64).unwrap();
    backend.write_at(0, &header.encode()).unwrap();

    let mut table = vec![0u8; table_bytes as usize];
    table[0..8].copy_from_slice(&data_offset.to_le_bytes());
    backend
        .write_at(crate::sparse::HEADER_SIZE as u64, &table)
        .unwrap();

    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_truncated_header() {
    // Empty backend: header read must fail, but `open` should not panic.
    let backend = MemBackend::new();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_zero_block_size() {
    let mut backend = MemBackend::new();
    let mut header = make_header(4096, 4096, 0);
    header.block_size_bytes = 0;
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_non_power_of_two_block_size() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 1536, 0); // multiple of 512 but not power-of-two
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_block_size_not_multiple_of_512() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 1025, 0);
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_block_size_too_large() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 32 * 1024 * 1024, 0); // 32 MiB
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(
        err,
        DiskError::InvalidSparseHeader("block_size too large")
    ));
}

#[test]
fn sparse_create_rejects_block_size_too_large() {
    let backend = MemBackend::new();
    let err = match AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes: 4096,
            block_size_bytes: 32 * 1024 * 1024,
        },
    ) {
        Ok(_) => panic!("expected create to fail"),
        Err(e) => e,
    };
    assert!(matches!(
        err,
        DiskError::InvalidSparseHeader("block_size too large")
    ));
}

#[test]
fn sparse_open_rejects_zero_disk_size() {
    let mut backend = MemBackend::new();
    let header = make_header(0, 4096, 0);
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_disk_size_not_multiple_of_512() {
    let mut backend = MemBackend::new();
    let header = make_header(4097, 4096, 0);
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_table_entries_mismatch() {
    let mut backend = MemBackend::new();
    let mut header = make_header(8192, 4096, 0);
    header.table_entries += 1;
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_bad_data_offset() {
    let mut backend = MemBackend::new();
    let mut header = make_header(8192, 4096, 0);
    header.data_offset -= 512;
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_allocated_blocks_gt_table_entries() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 4096, 2);
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn sparse_open_rejects_truncated_allocation_table() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 4096, 0);
    backend.write_at(0, &header.encode()).unwrap();
    // No table bytes written.
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_unaligned_table_entry() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 4096, 1);
    backend.write_at(0, &header.encode()).unwrap();
    write_table(&mut backend, &[header.data_offset + 1]);
    backend
        .set_len(header.data_offset + header.block_size_u64())
        .unwrap();

    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_table_entry_before_data_offset() {
    let mut backend = MemBackend::new();
    // Use a small block size + many table entries so `data_offset` is larger than a single
    // block and we can craft a block-aligned offset that still points into metadata.
    let header = make_header(512 * 100, 512, 1);
    backend.write_at(0, &header.encode()).unwrap();
    let phys = header.block_size_u64(); // aligned, non-zero, but < data_offset
    write_table(
        &mut backend,
        &std::iter::once(phys)
            .chain(std::iter::repeat_n(0, header.table_entries as usize - 1))
            .collect::<Vec<u64>>(),
    );

    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_table_entry_overflow() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 4096, 1);
    backend.write_at(0, &header.encode()).unwrap();
    let phys = u64::MAX - (header.block_size_u64() - 1);
    write_table(&mut backend, &[phys]);
    // No need to extend backend len; this should be rejected before checking bounds.
    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_table_entry_past_backend_end() {
    let mut backend = MemBackend::new();
    let header = make_header(4096, 4096, 1);
    backend.write_at(0, &header.encode()).unwrap();
    write_table(&mut backend, &[header.data_offset]);
    backend
        .set_len(header.data_offset + header.block_size_u64() - 1)
        .unwrap();

    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_duplicate_physical_offsets() {
    let mut backend = MemBackend::new();
    let header = make_header(8192, 4096, 2);
    backend.write_at(0, &header.encode()).unwrap();
    write_table(&mut backend, &[header.data_offset, header.data_offset]);
    backend
        .set_len(header.data_offset + 2 * header.block_size_u64())
        .unwrap();

    let err = open_sparse_err(backend);
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}

#[test]
fn sparse_open_rejects_huge_allocation_table() {
    let mut backend = MemBackend::new();
    // 1,000,000,000 entries => 8 GiB table.
    let header = make_header(512 * 1_000_000_000, 512, 0);
    backend.write_at(0, &header.encode()).unwrap();
    let err = open_sparse_err(backend);
    assert!(matches!(
        err,
        DiskError::Unsupported(_) | DiskError::InvalidSparseHeader(_)
    ));
}

#[cfg(target_pointer_width = "32")]
#[test]
fn sparse_is_block_allocated_does_not_truncate_block_idx() {
    let backend = MemBackend::new();
    let mut disk = AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();

    disk.write_at(0, &[1]).unwrap();
    assert!(disk.is_block_allocated(0));

    // On 32-bit targets, `u64 as usize` truncates; ensure we don't accidentally treat
    // this out-of-range block index as block 0.
    let big_idx = (u32::MAX as u64) + 1;
    assert!(!disk.is_block_allocated(big_idx));
}
