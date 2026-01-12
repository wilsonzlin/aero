use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend, VirtualDiskIsoBackend};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _};

#[test]
fn virtual_disk_iso_backend_reads_2048_byte_sectors() {
    // Two 2048-byte sectors.
    let mut disk = RawDisk::create(MemBackend::new(), 2 * AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    disk.write_at(AtapiCdrom::SECTOR_SIZE as u64, b"WORLD")
        .unwrap();

    let mut iso = VirtualDiskIsoBackend::new(Box::new(disk)).unwrap();
    assert_eq!(iso.sector_count(), 2);

    let mut buf = vec![0u8; AtapiCdrom::SECTOR_SIZE];
    iso.read_sectors(1, &mut buf).unwrap();
    assert_eq!(&buf[..5], b"WORLD");
}

#[test]
fn virtual_disk_iso_backend_rejects_unaligned_capacity() {
    let disk = RawDisk::create(MemBackend::new(), (AtapiCdrom::SECTOR_SIZE as u64) + 1).unwrap();
    let err = VirtualDiskIsoBackend::new(Box::new(disk)).err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn virtual_disk_iso_backend_rejects_unaligned_buffer_len() {
    let disk = RawDisk::create(MemBackend::new(), AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    let mut iso = VirtualDiskIsoBackend::new(Box::new(disk)).unwrap();

    let mut buf = [0u8; 1];
    let err = iso.read_sectors(0, &mut buf).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn virtual_disk_iso_backend_errors_on_out_of_bounds_read() {
    let disk = RawDisk::create(MemBackend::new(), AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    let mut iso = VirtualDiskIsoBackend::new(Box::new(disk)).unwrap();

    let mut buf = vec![0u8; AtapiCdrom::SECTOR_SIZE];
    let err = iso.read_sectors(1, &mut buf).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
}
