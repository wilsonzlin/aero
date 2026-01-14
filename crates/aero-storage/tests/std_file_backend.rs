#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{DiskError, StdFileBackend, StorageBackend};

#[cfg(any(unix, windows))]
use std::io::{Seek, SeekFrom, Write as _};

#[test]
fn std_file_backend_set_len_write_read_roundtrip() {
    let file = tempfile::tempfile().unwrap();
    let mut backend = StdFileBackend::from_file(file);

    backend.set_len(4096).unwrap();
    assert_eq!(backend.len().unwrap(), 4096);

    let data = b"hello std file backend";
    backend.write_at(123, data).unwrap();

    let mut back = vec![0u8; data.len()];
    backend.read_at(123, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn std_file_backend_sparse_large_offset_write() {
    let file = tempfile::tempfile().unwrap();
    let mut backend = StdFileBackend::from_file(file);

    let write_offset = 8 * 1024 * 1024; // 8 MiB hole before the write
    let data = vec![0x5Au8; 512];
    backend.write_at(write_offset, &data).unwrap();

    // File should grow to the end of the written region.
    assert_eq!(backend.len().unwrap(), write_offset + data.len() as u64);

    // Reading from the sparse hole should return zeros.
    let mut hole = [0xAAu8; 32];
    backend.read_at(0, &mut hole).unwrap();
    assert!(hole.iter().all(|b| *b == 0));

    let mut back = vec![0u8; data.len()];
    backend.read_at(write_offset, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn std_file_backend_read_oob_returns_out_of_bounds() {
    let file = tempfile::tempfile().unwrap();
    let mut backend = StdFileBackend::from_file(file);

    backend.set_len(1024).unwrap();

    let mut buf = [0u8; 200];
    let err = backend.read_at(900, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::OutOfBounds { .. }));
}

#[cfg(any(unix, windows))]
#[test]
fn std_file_backend_does_not_disturb_file_cursor() {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&[0u8; 16]).unwrap();
    file.seek(SeekFrom::Start(5)).unwrap();
    let before = file.stream_position().unwrap();

    let mut backend = StdFileBackend::from_file(file);
    let mut buf = [0u8; 4];
    backend.read_at(0, &mut buf).unwrap();
    backend.write_at(8, &[1, 2, 3, 4]).unwrap();
    backend.flush().unwrap();

    let mut file = backend.into_file();
    let after = file.stream_position().unwrap();
    assert_eq!(before, after);
}
