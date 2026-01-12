use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend, VirtualDiskIsoBackend};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _};

#[test]
fn virtual_disk_iso_backend_reads_2048_byte_sectors() {
    // Two 2048-byte sectors.
    let mut disk = RawDisk::create(MemBackend::new(), 2 * AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    disk.write_at(AtapiCdrom::SECTOR_SIZE as u64, b"WORLD").unwrap();

    let mut iso = VirtualDiskIsoBackend::new(Box::new(disk)).unwrap();
    assert_eq!(iso.sector_count(), 2);

    let mut buf = vec![0u8; AtapiCdrom::SECTOR_SIZE];
    iso.read_sectors(1, &mut buf).unwrap();
    assert_eq!(&buf[..5], b"WORLD");
}
