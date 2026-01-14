#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    detect_format, AeroSparseConfig, AeroSparseDisk, AeroSparseHeader, DiskError, DiskFormat,
    DiskImage, MemBackend, Qcow2Disk, StorageBackend, VhdDisk, VirtualDisk, SECTOR_SIZE,
};
use proptest::prelude::*;

const SECTOR: usize = SECTOR_SIZE;
const AEROSPAR_HEADER_SIZE: u64 = 64;
const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn vhd_footer_checksum(raw: &[u8; SECTOR]) -> u32 {
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

fn make_qcow2_empty(virtual_size: u64) -> MemBackend {
    assert_eq!(virtual_size % SECTOR as u64, 0);

    let cluster_bits = 16u32;
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;

    let file_len = cluster_size * 5;
    let mut storage = MemBackend::with_len(file_len).unwrap();

    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 3); // version
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, virtual_size);
    write_be_u32(&mut header, 36, 1); // l1_size
    write_be_u64(&mut header, 40, l1_table_offset);
    write_be_u64(&mut header, 48, refcount_table_offset);
    write_be_u32(&mut header, 56, 1); // refcount_table_clusters
    write_be_u64(&mut header, 72, 0); // incompatible_features
    write_be_u64(&mut header, 80, 0); // compatible_features
    write_be_u64(&mut header, 88, 0); // autoclear_features
    write_be_u32(&mut header, 96, 4); // refcount_order (16-bit)
    write_be_u32(&mut header, 100, 104); // header_length
    storage.write_at(0, &header).unwrap();

    storage
        .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
        .unwrap();

    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    storage
        .write_at(l1_table_offset, &l1_entry.to_be_bytes())
        .unwrap();

    for cluster_index in 0u64..5 {
        let off = refcount_block_offset + cluster_index * 2;
        storage.write_at(off, &1u16.to_be_bytes()).unwrap();
    }

    storage
}

fn make_qcow2_with_pattern() -> MemBackend {
    let virtual_size = 2 * 1024 * 1024;
    let cluster_size = 1u64 << 16;

    let mut storage = make_qcow2_empty(virtual_size);
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;
    storage.set_len(cluster_size * 6).unwrap();

    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
    storage
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();

    let refcount_block_offset = cluster_size * 3;
    storage
        .write_at(refcount_block_offset + 5 * 2, &1u16.to_be_bytes())
        .unwrap();

    let mut sector = [0u8; SECTOR];
    sector[..12].copy_from_slice(b"hello qcow2!");
    storage.write_at(data_cluster_offset, &sector).unwrap();

    storage
}

fn make_vhd_footer(virtual_size: u64, disk_type: u32, data_offset: u64) -> [u8; SECTOR] {
    let mut footer = [0u8; SECTOR];
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

fn make_vhd_fixed_with_pattern() -> MemBackend {
    let virtual_size = 1024 * 1024;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let mut storage = MemBackend::new();
    storage.write_at(0, &data).unwrap();
    storage.write_at(virtual_size, &footer).unwrap();
    storage
}

fn make_vhd_fixed_with_footer_copy() -> MemBackend {
    let virtual_size = 1024 * 1024u64;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);
    let mut footer_copy = footer;
    // Some implementations write a footer copy at offset 0 but do not keep all fields perfectly
    // in sync with the EOF footer. Mutate an unused field (original_size) so the copy is valid
    // but not byte-for-byte identical.
    write_be_u64(&mut footer_copy, 40, virtual_size + (SECTOR as u64));
    let checksum = vhd_footer_checksum(&footer_copy);
    write_be_u32(&mut footer_copy, 64, checksum);

    let mut storage = MemBackend::default();
    storage.write_at(0, &footer_copy).unwrap(); // footer copy at start
    storage.write_at(SECTOR as u64, &data).unwrap();
    storage
        .write_at((SECTOR as u64) + virtual_size, &footer)
        .unwrap();
    storage
}

fn make_vhd_fixed_without_footer_copy_but_sector0_looks_like_footer() -> MemBackend {
    // Construct a fixed VHD where sector 0 of the data region happens to resemble a valid VHD
    // footer (including a correct checksum). This should *not* be treated as an optional footer
    // copy, since the file length only includes a single required EOF footer.
    let virtual_size = 1024 * 1024u64;
    let mut data = vec![0u8; virtual_size as usize];

    // Sector 0: bytes that form a valid fixed-disk footer.
    let fake_footer = make_vhd_footer(virtual_size, 2, u64::MAX);
    data[..SECTOR].copy_from_slice(&fake_footer);

    // Sector 1: distinctive pattern so we can detect if the open path incorrectly skips sector 0.
    data[SECTOR..SECTOR + 8].copy_from_slice(b"PAYLOAD!");

    let eof_footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let mut storage = MemBackend::default();
    storage.write_at(0, &data).unwrap();
    storage.write_at(virtual_size, &eof_footer).unwrap();
    storage
}

fn make_vhd_dynamic_empty(virtual_size: u64, block_size: u32) -> MemBackend {
    assert_eq!(virtual_size % SECTOR as u64, 0);
    assert_eq!(block_size as usize % SECTOR, 0);

    let dyn_header_offset = SECTOR as u64;
    let table_offset = dyn_header_offset + 1024;
    let blocks = virtual_size.div_ceil(block_size as u64);
    let max_table_entries = blocks as u32;
    let bat_bytes = max_table_entries as u64 * 4;
    let bat_size = bat_bytes.div_ceil(SECTOR as u64) * (SECTOR as u64);

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    let file_len = (SECTOR as u64) + 1024 + bat_size + (SECTOR as u64);
    let mut storage = MemBackend::with_len(file_len).unwrap();

    storage.write_at(0, &footer).unwrap();
    storage
        .write_at(file_len - (SECTOR as u64), &footer)
        .unwrap();

    let mut dyn_header = [0u8; 1024];
    dyn_header[0..8].copy_from_slice(b"cxsparse");
    write_be_u64(&mut dyn_header, 8, u64::MAX);
    write_be_u64(&mut dyn_header, 16, table_offset);
    write_be_u32(&mut dyn_header, 24, 0x0001_0000);
    write_be_u32(&mut dyn_header, 28, max_table_entries);
    write_be_u32(&mut dyn_header, 32, block_size);
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    write_be_u32(&mut dyn_header, 36, checksum);
    storage.write_at(dyn_header_offset, &dyn_header).unwrap();

    let bat = vec![0xFFu8; bat_size as usize];
    storage.write_at(table_offset, &bat).unwrap();

    storage
}

fn make_vhd_dynamic_with_pattern() -> MemBackend {
    let virtual_size = 1024 * 1024;
    let block_size = 64 * 1024;
    let mut storage = make_vhd_dynamic_empty(virtual_size, block_size);

    let dyn_header_offset = SECTOR as u64;
    let table_offset = dyn_header_offset + 1024;
    let bat_size = SECTOR as u64;
    let old_footer_offset = (SECTOR as u64) + 1024 + bat_size;
    let bitmap_size = SECTOR as u64;
    let block_total_size = bitmap_size + block_size as u64;
    let new_footer_offset = old_footer_offset + block_total_size;

    storage
        .set_len(new_footer_offset + (SECTOR as u64))
        .unwrap();

    let bat_entry = (old_footer_offset / (SECTOR as u64)) as u32;
    storage
        .write_at(table_offset, &bat_entry.to_be_bytes())
        .unwrap();

    let mut bitmap = [0u8; SECTOR];
    bitmap[0] = 0x80;
    storage.write_at(old_footer_offset, &bitmap).unwrap();

    let mut sector = [0u8; SECTOR];
    sector[..12].copy_from_slice(b"hello vhd-d!");
    let data_offset = old_footer_offset + bitmap_size;
    storage.write_at(data_offset, &sector).unwrap();

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    storage.write_at(0, &footer).unwrap();
    storage.write_at(new_footer_offset, &footer).unwrap();

    storage
}

#[test]
fn detect_qcow2_and_vhd() {
    let mut qcow = make_qcow2_empty(1024 * 1024);
    assert_eq!(detect_format(&mut qcow).unwrap(), DiskFormat::Qcow2);

    let mut vhd = make_vhd_fixed_with_pattern();
    assert_eq!(detect_format(&mut vhd).unwrap(), DiskFormat::Vhd);
}

#[test]
fn detect_vhd_fixed_with_footer_copy() {
    let mut storage = make_vhd_fixed_with_footer_copy();
    assert_eq!(detect_format(&mut storage).unwrap(), DiskFormat::Vhd);

    let mut disk = DiskImage::open_auto(storage).unwrap();
    assert_eq!(disk.format(), DiskFormat::Vhd);

    let mut sector = [0u8; SECTOR];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");
}

#[test]
fn vhd_fixed_sector0_that_looks_like_footer_is_not_treated_as_footer_copy() {
    let mut storage = make_vhd_fixed_without_footer_copy_but_sector0_looks_like_footer();
    assert_eq!(detect_format(&mut storage).unwrap(), DiskFormat::Vhd);

    let mut disk = DiskImage::open_auto(storage).unwrap();
    assert_eq!(disk.format(), DiskFormat::Vhd);

    let mut sector0 = [0u8; SECTOR];
    disk.read_sectors(0, &mut sector0).unwrap();
    assert_eq!(&sector0[..8], b"conectix");

    let mut sector1 = [0u8; SECTOR];
    disk.read_sectors(1, &mut sector1).unwrap();
    assert_eq!(&sector1[..8], b"PAYLOAD!");
}

#[test]
fn detect_fixed_vhd_footer_at_offset0_without_eof_footer_is_raw() {
    // A file that begins with bytes resembling a fixed VHD footer should not be treated as VHD
    // unless it is large enough to plausibly contain both the optional footer copy and the
    // required EOF footer.
    let current_size = SECTOR as u64;
    let footer = make_vhd_footer(current_size, 2, u64::MAX);

    // footer at offset 0 + 512 bytes of data, but no EOF footer.
    let mut backend = MemBackend::with_len(current_size + (SECTOR as u64)).unwrap();
    backend.write_at(0, &footer).unwrap();
    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::Raw);
}

#[test]
fn detect_format_does_not_misclassify_vhd_cookie_without_valid_footer_fields() {
    // A random file that happens to contain "conectix" should not be treated as a VHD unless
    // the surrounding footer fields are also plausible.
    let mut backend = MemBackend::with_len(SECTOR as u64).unwrap();
    backend.write_at(0, b"conectix").unwrap();
    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::Raw);
}

#[test]
fn detect_format_recognizes_truncated_vhd_cookie() {
    // Truncated files that still begin with the VHD cookie should not silently open as raw.
    let mut backend = MemBackend::with_len(8).unwrap();
    backend.write_at(0, b"conectix").unwrap();

    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::Vhd);

    let err = DiskImage::open_auto(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::CorruptImage("vhd file too small")));
}

#[test]
fn detect_format_does_not_misclassify_qcow2_magic_with_bad_version() {
    // Use a file large enough to contain the minimum QCOW2 v2 header size (72 bytes). For smaller
    // files, we intentionally treat QCOW2 magic as truncated QCOW2 so callers get a useful error.
    let mut backend = MemBackend::with_len(72).unwrap();
    backend.write_at(0, b"QFI\xfb").unwrap();
    backend.write_at(4, &99u32.to_be_bytes()).unwrap(); // invalid version
    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::Raw);
}

#[test]
fn detect_format_recognizes_truncated_qcow2_magic() {
    // Truncated files that still begin with QCOW2 magic should not silently open as raw.
    let mut backend = MemBackend::with_len(4).unwrap();
    backend.write_at(0, b"QFI\xfb").unwrap();

    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::Qcow2);

    let err = DiskImage::open_auto(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 header truncated")
    ));
}

#[test]
fn detect_format_does_not_misclassify_aerosparse_magic_with_bad_header() {
    let mut backend = MemBackend::with_len(64).unwrap();
    backend.write_at(0, b"AEROSPAR").unwrap();
    // Version is still zero, so this is not a valid AeroSparse header.
    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::Raw);
}

#[test]
fn detect_format_recognizes_aerosparse_magic_with_minimally_plausible_header() {
    // Format detection should be less strict than full header validation: a file that looks like
    // AeroSparse should be detected as such and then fail open with a structured error, instead
    // of silently falling back to Raw.
    let mut backend = MemBackend::with_len(64).unwrap();
    let mut header = [0u8; 64];
    header[..8].copy_from_slice(b"AEROSPAR");
    header[8..12].copy_from_slice(&1u32.to_le_bytes()); // version
    header[12..16].copy_from_slice(&64u32.to_le_bytes()); // header_size
    header[32..40].copy_from_slice(&64u64.to_le_bytes()); // table_offset

    // Leave the rest of the header zero so it fails full validation (block_size_bytes=0).
    backend.write_at(0, &header).unwrap();

    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::AeroSparse);

    let err = DiskImage::open_auto(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::InvalidSparseHeader(_)));
}

#[test]
fn detect_format_recognizes_aerosparse_magic_with_bad_table_offset() {
    let mut backend = MemBackend::with_len(64).unwrap();
    let mut header = [0u8; 64];
    header[..8].copy_from_slice(b"AEROSPAR");
    header[8..12].copy_from_slice(&1u32.to_le_bytes()); // version
    header[12..16].copy_from_slice(&64u32.to_le_bytes()); // header_size
    header[32..40].copy_from_slice(&0u64.to_le_bytes()); // bad table_offset
    backend.write_at(0, &header).unwrap();

    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::AeroSparse);

    let err = DiskImage::open_auto(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::InvalidSparseHeader("unsupported table offset")
    ));
}

#[test]
fn detect_format_recognizes_truncated_aerosparse_magic() {
    // Truncated files that still begin with the AeroSparse magic should not silently open as raw.
    let mut backend = MemBackend::with_len(8).unwrap();
    backend.write_at(0, b"AEROSPAR").unwrap();

    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::AeroSparse);

    let err = DiskImage::open_auto(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptSparseImage("truncated sparse header")
    ));
}

#[test]
fn detect_aerosparse_and_raw() {
    let sparse = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    let mut sparse_backend = sparse.into_backend();
    assert_eq!(
        detect_format(&mut sparse_backend).unwrap(),
        DiskFormat::AeroSparse
    );

    let mut raw = MemBackend::with_len(16).unwrap();
    assert_eq!(detect_format(&mut raw).unwrap(), DiskFormat::Raw);
}

#[test]
fn open_auto_works_for_aerosparse_and_raw() {
    let sparse = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    let disk = DiskImage::open_auto(sparse.into_backend()).unwrap();
    assert_eq!(disk.format(), DiskFormat::AeroSparse);

    let raw = MemBackend::with_len(16).unwrap();
    let disk = DiskImage::open_auto(raw).unwrap();
    assert_eq!(disk.format(), DiskFormat::Raw);
}

#[test]
fn aerosparse_rejects_table_entry_before_data_region() {
    let disk = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    let header = *disk.header();
    let block_size = header.block_size_u64();

    let mut backend = disk.into_backend();

    // Pretend the image has allocated blocks, then inject an invalid table entry that points
    // into the header/table region.
    let mut bad_header = header;
    bad_header.allocated_blocks = 1;
    backend.set_len(header.data_offset + block_size).unwrap();
    backend.write_at(0, &bad_header.encode()).unwrap();
    backend
        .write_at(AEROSPAR_HEADER_SIZE, &(SECTOR as u64).to_le_bytes())
        .unwrap();

    match AeroSparseDisk::open(backend) {
        Ok(_) => panic!("expected open to fail"),
        Err(err) => assert!(matches!(
            err,
            DiskError::CorruptSparseImage("data block offset before data region")
        )),
    }
}

#[test]
fn aerosparse_rejects_misaligned_table_entry() {
    let disk = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    let header = *disk.header();
    let block_size = header.block_size_u64();

    let mut backend = disk.into_backend();
    let mut bad_header = header;
    bad_header.allocated_blocks = 1;
    backend.set_len(header.data_offset + block_size).unwrap();
    backend.write_at(0, &bad_header.encode()).unwrap();
    backend
        .write_at(
            AEROSPAR_HEADER_SIZE,
            &(header.data_offset + (SECTOR as u64)).to_le_bytes(),
        )
        .unwrap();

    match AeroSparseDisk::open(backend) {
        Ok(_) => panic!("expected open to fail"),
        Err(err) => assert!(matches!(
            err,
            DiskError::CorruptSparseImage("misaligned data block offset")
        )),
    }
}

#[test]
fn aerosparse_rejects_table_entry_pointing_past_allocated_region() {
    let disk = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    let header = *disk.header();
    let block_size = header.block_size_u64();

    let mut backend = disk.into_backend();
    let mut bad_header = header;
    bad_header.allocated_blocks = 1;
    backend.set_len(header.data_offset + block_size).unwrap();
    backend.write_at(0, &bad_header.encode()).unwrap();
    backend
        .write_at(
            AEROSPAR_HEADER_SIZE,
            &(header.data_offset + block_size).to_le_bytes(),
        )
        .unwrap();

    match AeroSparseDisk::open(backend) {
        Ok(_) => panic!("expected open to fail"),
        Err(err) => assert!(matches!(
            err,
            DiskError::CorruptSparseImage("data block offset out of bounds")
        )),
    }
}

#[test]
fn aerosparse_rejects_absurd_allocation_table_sizes() {
    // Trigger the hard cap in `AeroSparseDisk::open` without allocating huge memory.
    let table_entries: u64 = (128 * 1024 * 1024 / 8) + 1;
    let block_size_bytes: u32 = SECTOR as u32;
    let disk_size_bytes = table_entries * block_size_bytes as u64;

    let header = AeroSparseHeader {
        version: 1,
        block_size_bytes,
        disk_size_bytes,
        table_entries,
        // Invalid, but `open()` must reject based on table size before validating data_offset.
        data_offset: 0,
        allocated_blocks: 0,
    };

    let mut backend = MemBackend::with_len(AEROSPAR_HEADER_SIZE).unwrap();
    backend.write_at(0, &header.encode()).unwrap();

    match AeroSparseDisk::open(backend) {
        Ok(_) => panic!("expected open to fail"),
        Err(err) => assert!(matches!(
            err,
            DiskError::Unsupported("aerosparse allocation table too large")
        )),
    }
}

#[test]
fn aerosparse_rejects_table_entries_mismatch() {
    let disk = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    let header = *disk.header();
    let mut backend = disk.into_backend();

    // Corrupt the header with an inconsistent table_entries.
    let mut bad_header = header;
    bad_header.table_entries += 1;
    backend.write_at(0, &bad_header.encode()).unwrap();

    match AeroSparseDisk::open(backend) {
        Ok(_) => panic!("expected open to fail"),
        Err(err) => assert!(matches!(
            err,
            DiskError::InvalidSparseHeader("unexpected table_entries")
        )),
    }
}

#[test]
fn aerosparse_rejects_allocated_blocks_exceeding_table_entries() {
    let disk = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: 4096,
        },
    )
    .unwrap();
    let header = *disk.header();
    let mut backend = disk.into_backend();

    // Claim more allocated blocks than there are table entries.
    let mut bad_header = header;
    bad_header.allocated_blocks = bad_header.table_entries + 1;
    backend.write_at(0, &bad_header.encode()).unwrap();

    match AeroSparseDisk::open(backend) {
        Ok(_) => panic!("expected open to fail"),
        Err(err) => assert!(matches!(
            err,
            DiskError::InvalidSparseHeader("allocated_blocks exceeds table_entries")
        )),
    }
}

#[test]
fn aerosparse_create_rejects_absurd_allocation_table_sizes() {
    let table_entries: u64 = (128 * 1024 * 1024 / 8) + 1;
    let block_size_bytes: u32 = SECTOR as u32;
    let disk_size_bytes = table_entries * block_size_bytes as u64;

    match AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes,
            block_size_bytes,
        },
    ) {
        Ok(_) => panic!("expected create to fail"),
        Err(err) => assert!(matches!(
            err,
            DiskError::InvalidConfig("aerosparse allocation table too large")
        )),
    }
}

#[test]
fn qcow2_rejects_l1_entries_pointing_past_eof() {
    let virtual_size = 1024 * 1024;
    let mut storage = make_qcow2_empty(virtual_size);

    // The fixture uses cluster_bits=16.
    let cluster_size = 1u64 << 16;
    let l1_table_offset = cluster_size * 2;
    let bad_l2_table_offset = cluster_size * 1000; // well beyond EOF
    let bad_l1_entry = bad_l2_table_offset | QCOW2_OFLAG_COPIED;
    storage
        .write_at(l1_table_offset, &bad_l1_entry.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(storage).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 l2 table truncated")
    ));
}

#[test]
fn qcow2_rejects_incompatible_features() {
    let virtual_size = 1024 * 1024;
    let mut storage = make_qcow2_empty(virtual_size);

    // incompatible_features is at offset 72 in the v3 header extension.
    storage.write_at(72, &1u64.to_be_bytes()).unwrap();

    let err = Qcow2Disk::open(storage).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported(_)));
}

#[test]
fn qcow2_unallocated_reads_zero() {
    let storage = make_qcow2_empty(1024 * 1024);
    let mut disk = DiskImage::open_auto(storage).unwrap();
    assert_eq!(disk.format(), DiskFormat::Qcow2);

    let mut buf = vec![0xAAu8; SECTOR * 4];
    disk.read_sectors(0, &mut buf).unwrap();
    assert!(buf.iter().all(|b| *b == 0));
}

#[test]
fn qcow2_fixture_read_and_write() {
    let storage = make_qcow2_with_pattern();
    let mut disk = DiskImage::open_auto(storage).unwrap();

    let mut sector = [0u8; SECTOR];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..12], b"hello qcow2!");

    let mut write_buf = vec![0u8; SECTOR];
    write_buf[..14].copy_from_slice(b"write qcow2 ok");
    disk.write_sectors(10, &write_buf).unwrap();

    let mut read_back = vec![0u8; SECTOR];
    disk.read_sectors(10, &mut read_back).unwrap();
    assert_eq!(read_back, write_buf);
}

#[test]
fn qcow2_write_persists_after_reopen() {
    let storage = make_qcow2_empty(1024 * 1024);
    let mut disk = Qcow2Disk::open(storage).unwrap();

    let data = vec![0x5Au8; SECTOR * 2];
    disk.write_sectors(1, &data).unwrap();
    disk.flush().unwrap();

    let storage = disk.into_backend();
    let mut reopened = Qcow2Disk::open(storage).unwrap();
    let mut back = vec![0u8; SECTOR * 2];
    reopened.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_fixed_fixture_read() {
    let storage = make_vhd_fixed_with_pattern();
    let mut disk = DiskImage::open_auto(storage).unwrap();
    assert_eq!(disk.format(), DiskFormat::Vhd);

    let mut sector = [0u8; SECTOR];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");
}

#[test]
fn vhd_rejects_bat_entries_pointing_past_eof() {
    let virtual_size = 1024 * 1024;
    let block_size = 64 * 1024;
    let mut storage = make_vhd_dynamic_empty(virtual_size, block_size);

    // The fixture writes the BAT at this fixed offset.
    let table_offset = (SECTOR as u64) + 1024;
    let bad_sector = 0x10_0000u32; // points far past EOF
    storage
        .write_at(table_offset, &bad_sector.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(storage).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd block overlaps footer")
    ));
}

#[test]
fn vhd_rejects_bad_footer_checksum() {
    let mut storage = make_vhd_fixed_with_pattern();

    // Corrupt a byte in the footer (but keep the cookie intact) so the checksum no longer matches.
    let footer_offset = 1024 * 1024;
    let mut footer = [0u8; SECTOR];
    storage.read_at(footer_offset, &mut footer).unwrap();
    footer[8] ^= 0x01;
    storage.write_at(footer_offset, &footer).unwrap();

    // Even when the checksum is wrong, format detection should still classify the image as a VHD so
    // `open_auto` reports a corruption error instead of silently treating it as a raw disk.
    assert_eq!(detect_format(&mut storage).unwrap(), DiskFormat::Vhd);

    let err = DiskImage::open_auto(storage).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd footer checksum mismatch")
    ));
}

#[test]
fn vhd_dynamic_unallocated_reads_zero_and_writes_allocate() {
    let storage = make_vhd_dynamic_empty(1024 * 1024, 64 * 1024);
    let mut disk = DiskImage::open_auto(storage).unwrap();

    let mut buf = vec![0xAAu8; SECTOR * 8];
    disk.read_sectors(0, &mut buf).unwrap();
    assert!(buf.iter().all(|b| *b == 0));

    let data = vec![0x5Au8; SECTOR * 2];
    disk.write_sectors(1, &data).unwrap();
    let mut back = vec![0u8; SECTOR * 2];
    disk.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_dynamic_fixture_read() {
    let storage = make_vhd_dynamic_with_pattern();
    let mut disk = DiskImage::open_auto(storage).unwrap();

    let mut sector = [0u8; SECTOR];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..12], b"hello vhd-d!");
}

#[test]
fn vhd_dynamic_write_persists_after_reopen() {
    let storage = make_vhd_dynamic_empty(1024 * 1024, 64 * 1024);
    let mut disk = VhdDisk::open(storage).unwrap();

    let data = vec![0xCCu8; SECTOR];
    disk.write_sectors(3, &data).unwrap();
    disk.flush().unwrap();

    let storage = disk.into_backend();
    let mut reopened = VhdDisk::open(storage).unwrap();
    let mut back = vec![0u8; SECTOR];
    reopened.read_sectors(3, &mut back).unwrap();
    assert_eq!(back, data);
}

#[derive(Clone, Debug)]
enum Op {
    Read { lba: u64, sectors: usize },
    Write { lba: u64, data: Vec<u8> },
}

fn ops_strategy(capacity_sectors: u64) -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(
        (0u64..capacity_sectors).prop_flat_map(move |lba| {
            let max = (capacity_sectors - lba) as usize;
            let max_sectors = max.clamp(1, 4);
            (1usize..=max_sectors).prop_flat_map(move |sectors| {
                prop_oneof![
                    3 => Just(Op::Read { lba, sectors }),
                    7 => proptest::collection::vec(any::<u8>(), sectors * SECTOR)
                        .prop_map(move |data| Op::Write { lba, data }),
                ]
            })
        }),
        1..50,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn prop_qcow2_matches_reference(ops in ops_strategy(128)) {
        let capacity_sectors = 128u64;
        let virtual_size = capacity_sectors * SECTOR as u64;
        let storage = make_qcow2_empty(virtual_size);
        let mut disk = DiskImage::open_with_format(DiskFormat::Qcow2, storage).unwrap();
        let mut reference = vec![0u8; virtual_size as usize];

        for op in ops {
            match op {
                Op::Read { lba, sectors } => {
                    let mut buf = vec![0u8; sectors * SECTOR];
                    disk.read_sectors(lba, &mut buf).unwrap();
                    let start = (lba * SECTOR as u64) as usize;
                    prop_assert_eq!(&buf[..], &reference[start..start + buf.len()]);
                }
                Op::Write { lba, data } => {
                    disk.write_sectors(lba, &data).unwrap();
                    let start = (lba * SECTOR as u64) as usize;
                    reference[start..start + data.len()].copy_from_slice(&data);
                }
            }
        }
    }

    #[test]
    fn prop_vhd_dynamic_matches_reference(ops in ops_strategy(128)) {
        let capacity_sectors = 128u64;
        let virtual_size = capacity_sectors * SECTOR as u64;
        let storage = make_vhd_dynamic_empty(virtual_size, 64 * 1024);
        let mut disk = DiskImage::open_with_format(DiskFormat::Vhd, storage).unwrap();
        let mut reference = vec![0u8; virtual_size as usize];

        for op in ops {
            match op {
                Op::Read { lba, sectors } => {
                    let mut buf = vec![0u8; sectors * SECTOR];
                    disk.read_sectors(lba, &mut buf).unwrap();
                    let start = (lba * SECTOR as u64) as usize;
                    prop_assert_eq!(&buf[..], &reference[start..start + buf.len()]);
                }
                Op::Write { lba, data } => {
                    disk.write_sectors(lba, &data).unwrap();
                    let start = (lba * SECTOR as u64) as usize;
                    reference[start..start + data.len()].copy_from_slice(&data);
                }
            }
        }
    }
}
