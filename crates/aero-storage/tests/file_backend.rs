#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, DiskError, DiskFormat, DiskImage, FileBackend, MemBackend,
    StorageBackend as _, VirtualDisk, SECTOR_SIZE,
};
use tempfile::tempdir;

const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn make_qcow2_with_pattern() -> MemBackend {
    let virtual_size = 2 * 1024 * 1024;
    let cluster_bits = 12u32; // 4 KiB clusters
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;

    let file_len = cluster_size * 5;
    let mut backend = MemBackend::with_len(file_len).unwrap();

    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 3); // version
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, virtual_size);
    write_be_u32(&mut header, 36, 1); // l1_size
    write_be_u64(&mut header, 40, l1_table_offset);
    write_be_u64(&mut header, 48, refcount_table_offset);
    write_be_u32(&mut header, 56, 1); // refcount_table_clusters
    write_be_u32(&mut header, 96, 4); // refcount_order (16-bit)
    write_be_u32(&mut header, 100, 104); // header_length
    backend.write_at(0, &header).unwrap();

    backend
        .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
        .unwrap();

    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l1_table_offset, &l1_entry.to_be_bytes())
        .unwrap();

    // Mark metadata clusters as in-use: header, refcount table, L1 table, refcount block, L2 table.
    for cluster_index in 0u64..5 {
        let off = refcount_block_offset + cluster_index * 2;
        backend.write_at(off, &1u16.to_be_bytes()).unwrap();
    }

    // Allocate a data cluster for guest cluster 0.
    let data_cluster_offset = cluster_size * 5;
    backend.set_len(cluster_size * 6).unwrap();
    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();
    backend
        .write_at(refcount_block_offset + 5 * 2, &1u16.to_be_bytes())
        .unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    sector[..12].copy_from_slice(b"hello qcow2!");
    backend.write_at(data_cluster_offset, &sector).unwrap();

    backend
}

fn vhd_footer_checksum(raw: &[u8; SECTOR_SIZE]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in raw.iter().enumerate() {
        if (64..68).contains(&i) {
            continue;
        }
        sum = sum.wrapping_add(*b as u32);
    }
    !sum
}

fn make_vhd_footer(virtual_size: u64, disk_type: u32, data_offset: u64) -> [u8; SECTOR_SIZE] {
    let mut footer = [0u8; SECTOR_SIZE];
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
    let virtual_size = 64 * 1024;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let mut backend = MemBackend::default();
    backend.write_at(0, &data).unwrap();
    backend.write_at(virtual_size, &footer).unwrap();
    backend
}

#[test]
fn file_backend_open_and_read_at() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    std::fs::write(&path, b"abcdef").unwrap();

    let mut backend = FileBackend::open_read_only(&path).unwrap();
    assert_eq!(backend.len().unwrap(), 6);

    let mut buf = [0u8; 2];
    backend.read_at(2, &mut buf).unwrap();
    assert_eq!(&buf, b"cd");
}

#[test]
fn file_backend_write_at_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let mut backend = FileBackend::create(&path, 16).unwrap();
    backend.write_at(0, b"hello world").unwrap();
    backend.write_at(6, b"WORLD").unwrap();

    let mut buf = [0u8; 11];
    backend.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, b"hello WORLD");
}

#[test]
fn file_backend_set_len_grows_and_shrinks() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let mut backend = FileBackend::create(&path, 8).unwrap();
    assert_eq!(backend.len().unwrap(), 8);

    backend.set_len(32).unwrap();
    assert_eq!(backend.len().unwrap(), 32);

    backend.set_len(4).unwrap();
    assert_eq!(backend.len().unwrap(), 4);

    let mut buf = [0u8; 2];
    let err = backend.read_at(3, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::OutOfBounds { .. }));
}

#[test]
fn file_backend_read_beyond_eof_is_out_of_bounds() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let mut backend = FileBackend::create(&path, 4).unwrap();
    backend.write_at(0, &[1, 2, 3, 4]).unwrap();

    let mut buf = [0u8; 2];
    let err = backend.read_at(3, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::OutOfBounds { .. }));
}

#[test]
fn file_backend_can_open_disk_image_auto() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let backend = FileBackend::create(&path, (SECTOR_SIZE * 8) as u64).unwrap();
    let mut disk = DiskImage::open_auto(backend).unwrap();
    assert_eq!(disk.format(), DiskFormat::Raw);

    let sector = vec![0xA5u8; SECTOR_SIZE];
    disk.write_sectors(0, &sector).unwrap();
    disk.flush().unwrap();

    // Ensure data persists after reopening.
    let backend = FileBackend::open_rw(&path).unwrap();
    let mut disk = DiskImage::open_auto(backend).unwrap();
    let mut buf = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut buf).unwrap();
    assert_eq!(buf, sector);
}

#[test]
fn file_backend_write_extends_file_and_zero_fills_gap() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let mut backend = FileBackend::create(&path, 4).unwrap();
    backend.write_at(6, &[0xAA, 0xBB]).unwrap();
    assert_eq!(backend.len().unwrap(), 8);

    // The gap created by extending the file should read as zeros.
    let mut gap = [0xFFu8; 2];
    backend.read_at(4, &mut gap).unwrap();
    assert_eq!(gap, [0, 0]);

    let mut tail = [0u8; 2];
    backend.read_at(6, &mut tail).unwrap();
    assert_eq!(tail, [0xAA, 0xBB]);
}

#[test]
fn file_backend_aerospar_disk_persists_after_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.aerospar");

    {
        let backend = FileBackend::create(&path, 0).unwrap();
        let mut disk = AeroSparseDisk::create(
            backend,
            AeroSparseConfig {
                disk_size_bytes: (SECTOR_SIZE * 128) as u64,
                block_size_bytes: 4096,
            },
        )
        .unwrap();

        disk.write_at(123, &[9, 8, 7, 6]).unwrap();
        disk.flush().unwrap();
    }

    let backend = FileBackend::open_rw(&path).unwrap();
    let mut disk = DiskImage::open_auto(backend).unwrap();
    assert_eq!(disk.format(), DiskFormat::AeroSparse);

    let mut back = [0u8; 4];
    disk.read_at(123, &mut back).unwrap();
    assert_eq!(back, [9, 8, 7, 6]);
}

#[test]
fn file_backend_open_auto_detects_qcow2() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");

    let bytes = make_qcow2_with_pattern().into_vec();
    std::fs::write(&path, bytes).unwrap();

    let backend = FileBackend::open_read_only(&path).unwrap();
    let mut disk = DiskImage::open_auto(backend).unwrap();
    assert_eq!(disk.format(), DiskFormat::Qcow2);

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..12], b"hello qcow2!");
}

#[test]
fn file_backend_open_auto_detects_vhd() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.vhd");

    let bytes = make_vhd_fixed_with_pattern().into_vec();
    std::fs::write(&path, bytes).unwrap();

    let backend = FileBackend::open_read_only(&path).unwrap();
    let mut disk = DiskImage::open_auto(backend).unwrap();
    assert_eq!(disk.format(), DiskFormat::Vhd);

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");
}

#[test]
fn file_backend_qcow2_write_persists_after_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.qcow2");

    let bytes = make_qcow2_with_pattern().into_vec();
    std::fs::write(&path, bytes).unwrap();

    let original_len = std::fs::metadata(&path).unwrap().len();

    let write_buf = vec![0xCCu8; SECTOR_SIZE];

    {
        let backend = FileBackend::open_rw(&path).unwrap();
        let mut disk = DiskImage::open_auto(backend).unwrap();
        assert_eq!(disk.format(), DiskFormat::Qcow2);

        // Writing to a different cluster should allocate new storage and grow the file.
        disk.write_sectors(8, &write_buf).unwrap();
        disk.flush().unwrap();

        let mut backend = disk.into_backend();
        let new_len = backend.len().unwrap();
        assert!(new_len > original_len);
    }

    // Ensure data persists after reopening.
    let backend = FileBackend::open_read_only(&path).unwrap();
    let mut disk = DiskImage::open_auto(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(8, &mut back).unwrap();
    assert_eq!(back, write_buf);
}

#[test]
fn file_backend_vhd_fixed_write_persists_after_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.vhd");

    let bytes = make_vhd_fixed_with_pattern().into_vec();
    std::fs::write(&path, bytes).unwrap();

    let original_len = std::fs::metadata(&path).unwrap().len();

    let write_buf = vec![0xDDu8; SECTOR_SIZE];

    {
        let backend = FileBackend::open_rw(&path).unwrap();
        let mut disk = DiskImage::open_auto(backend).unwrap();
        assert_eq!(disk.format(), DiskFormat::Vhd);

        disk.write_sectors(1, &write_buf).unwrap();
        disk.flush().unwrap();

        let mut backend = disk.into_backend();
        let new_len = backend.len().unwrap();
        assert_eq!(new_len, original_len);
    }

    // Ensure data persists after reopening.
    let backend = FileBackend::open_read_only(&path).unwrap();
    let mut disk = DiskImage::open_auto(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, write_buf);
}

#[test]
fn file_backend_read_only_rejects_writes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let mut backend = FileBackend::create(&path, 4).unwrap();
    backend.write_at(0, &[1, 2, 3, 4]).unwrap();
    backend.flush().unwrap();

    let mut backend = FileBackend::open_read_only(&path).unwrap();
    backend.flush().unwrap();
    let err = backend.write_at(0, &[9]).unwrap_err();
    assert!(matches!(
        err,
        DiskError::NotSupported(msg) if msg == "read-only backend"
    ));

    let err = backend.set_len(8).unwrap_err();
    assert!(matches!(
        err,
        DiskError::NotSupported(msg) if msg == "read-only backend"
    ));
}

#[test]
fn file_backend_reports_offset_overflow() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let mut backend = FileBackend::create(&path, 4).unwrap();

    let mut buf = [0u8; 1];
    let err = backend.read_at(u64::MAX, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::OffsetOverflow));

    let err = backend.write_at(u64::MAX, &buf).unwrap_err();
    assert!(matches!(err, DiskError::OffsetOverflow));
}
