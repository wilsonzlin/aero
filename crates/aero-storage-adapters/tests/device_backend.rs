use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use aero_storage_adapters::AeroVirtualDiskAsDeviceBackend;

#[test]
fn device_backend_enforces_sector_alignment() {
    let disk = RawDisk::create(MemBackend::with_len(4096).unwrap(), 4096).unwrap();
    let backend = AeroVirtualDiskAsDeviceBackend::new(Box::new(disk) as Box<dyn VirtualDisk + Send>);

    let mut buf = vec![0u8; SECTOR_SIZE];

    let err = backend.read_at_aligned(1, &mut buf).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    let mut bad_len = vec![0u8; SECTOR_SIZE - 1];
    let err = backend.read_at_aligned(0, &mut bad_len).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn device_backend_reports_out_of_bounds_as_unexpected_eof() {
    let disk = RawDisk::create(MemBackend::with_len(1024).unwrap(), 1024).unwrap();
    let backend = AeroVirtualDiskAsDeviceBackend::new(Box::new(disk) as Box<dyn VirtualDisk + Send>);

    let mut buf = vec![0u8; SECTOR_SIZE];
    let err = backend.read_at_aligned(1024, &mut buf).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn device_backend_read_write_roundtrip() {
    let disk = RawDisk::create(MemBackend::with_len(1024).unwrap(), 1024).unwrap();
    let backend = AeroVirtualDiskAsDeviceBackend::new(Box::new(disk) as Box<dyn VirtualDisk + Send>);

    let data = vec![0xA5u8; SECTOR_SIZE];
    backend.write_at_aligned(0, &data).unwrap();
    backend.flush().unwrap();

    let mut back = vec![0u8; SECTOR_SIZE];
    backend.read_at_aligned(0, &mut back).unwrap();
    assert_eq!(back, data);
}

