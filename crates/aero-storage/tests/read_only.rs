use aero_storage::{
    DiskError, DiskImage, MemBackend, RawDisk, ReadOnlyBackend, ReadOnlyDisk, StorageBackend,
    VirtualDisk,
};

#[test]
fn read_only_disk_allows_reads_and_rejects_writes() {
    let mut raw = RawDisk::create(MemBackend::new(), 4096).unwrap();
    raw.write_at(0, b"test").unwrap();

    let mut disk = ReadOnlyDisk::new(raw);

    let mut buf = [0u8; 4];
    disk.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, b"test");

    let err = disk.write_at(0, b"x").unwrap_err();
    assert!(matches!(err, DiskError::NotSupported(s) if s == "read-only"));
}

#[test]
fn read_only_backend_rejects_mutations() {
    let mut backend = ReadOnlyBackend::new(MemBackend::from_vec(vec![1, 2, 3, 4]));

    assert_eq!(backend.len().unwrap(), 4);
    let mut buf = [0u8; 4];
    backend.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, &[1, 2, 3, 4]);

    let err = backend.set_len(8).unwrap_err();
    assert!(matches!(err, DiskError::NotSupported(s) if s == "read-only"));

    let err = backend.write_at(0, &[9]).unwrap_err();
    assert!(matches!(err, DiskError::NotSupported(s) if s == "read-only"));

    let err = backend.flush().unwrap_err();
    assert!(matches!(err, DiskError::NotSupported(s) if s == "read-only"));
}

#[test]
fn disk_image_open_auto_can_be_wrapped_read_only() {
    let backend = MemBackend::with_len(1024 * 1024).unwrap();
    let img = DiskImage::open_auto(backend).unwrap();

    let mut ro = ReadOnlyDisk::new(img);
    assert_eq!(ro.capacity_bytes(), 1024 * 1024);

    let err = ro.write_at(0, &[1]).unwrap_err();
    assert!(matches!(err, DiskError::NotSupported(s) if s == "read-only"));
}

