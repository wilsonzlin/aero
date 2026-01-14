#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, DiskError, DiskFormat, DiskImage, FileBackend,
    StorageBackend as _, VirtualDisk, SECTOR_SIZE,
};
use tempfile::tempdir;

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
