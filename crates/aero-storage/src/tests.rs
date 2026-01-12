use crate::{
    AeroCowDisk, AeroSparseConfig, AeroSparseDisk, AeroSparseHeader, BlockCachedDisk, DiskError,
    MemBackend, RawDisk, StorageBackend, VirtualDisk, SECTOR_SIZE,
};

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
fn boxed_virtual_disk_can_be_used_in_generic_wrappers() {
    let raw = RawDisk::create(MemBackend::new(), (SECTOR_SIZE * 8) as u64).unwrap();
    let boxed: Box<dyn VirtualDisk> = Box::new(raw);

    // This test is primarily a compile-time check that `Box<dyn VirtualDisk>` implements
    // `VirtualDisk` (so it can be used in generic wrappers like `BlockCachedDisk`).
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
    // The backend only contains the header; `open` must reject the image without
    // allocating the claimed table size.
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
        table_entries: table_entries as u64,
        data_offset,
        allocated_blocks: 0,
    };

    let mut backend = MemBackend::new();
    backend.write_at(0, &header.encode()).unwrap();

    let err = AeroSparseDisk::open(backend).err().unwrap();
    assert!(matches!(err, DiskError::Unsupported(_) | DiskError::InvalidSparseHeader(_)));
}

#[cfg(target_pointer_width = "32")]
#[test]
fn sparse_open_rejects_table_size_overflow_usize() {
    // On 32-bit targets, converting the claimed table size to `usize` must be checked.
    // This image claims a table of (usize::MAX + 1) bytes.
    let block_size_bytes = 4096u32;
    let table_entries = (usize::MAX as u64 / 8) + 1;
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

    let err = AeroSparseDisk::open(backend).err().unwrap();
    assert!(matches!(err, DiskError::Unsupported(_) | DiskError::InvalidSparseHeader(_)));
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

    // Create a backend large enough to cover the one allocated block we reference.
    let mut backend = MemBackend::with_len(data_offset + block_size_bytes as u64).unwrap();
    backend.write_at(0, &header.encode()).unwrap();

    let mut table = vec![0u8; table_bytes as usize];
    table[0..8].copy_from_slice(&data_offset.to_le_bytes());
    backend
        .write_at(crate::sparse::HEADER_SIZE as u64, &table)
        .unwrap();

    let err = AeroSparseDisk::open(backend).err().unwrap();
    assert!(matches!(err, DiskError::CorruptSparseImage(_)));
}
