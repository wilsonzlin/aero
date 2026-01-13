#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{DiskError, StdFileBackend, StorageBackend as _};
use tempfile::tempdir;

#[test]
fn file_backend_open_and_read_at() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    std::fs::write(&path, b"abcdef").unwrap();

    let mut backend = StdFileBackend::open(&path, true).unwrap();
    assert_eq!(backend.len().unwrap(), 6);

    let mut buf = [0u8; 2];
    backend.read_at(2, &mut buf).unwrap();
    assert_eq!(&buf, b"cd");
}

#[test]
fn file_backend_write_at_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disk.img");

    let mut backend = StdFileBackend::create(&path, 16).unwrap();
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

    let mut backend = StdFileBackend::create(&path, 8).unwrap();
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

    let mut backend = StdFileBackend::create(&path, 4).unwrap();
    backend.write_at(0, &[1, 2, 3, 4]).unwrap();

    let mut buf = [0u8; 2];
    let err = backend.read_at(3, &mut buf).unwrap_err();
    assert!(matches!(err, DiskError::OutOfBounds { .. }));
}

