use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_machine::SharedIsoDisk;
use aero_storage::{MemBackend, RawDisk, VirtualDisk};
use firmware::bios::{CdromDevice, DiskError, CDROM_SECTOR_SIZE};

#[test]
fn shared_iso_can_be_cloned_and_read_via_firmware_and_atapi_traits() {
    let iso_capacity = 2 * AtapiCdrom::SECTOR_SIZE as u64;
    let mut iso_disk = RawDisk::create(MemBackend::new(), iso_capacity).unwrap();
    iso_disk.write_at(0, b"HELLO").unwrap();
    iso_disk
        .write_at(AtapiCdrom::SECTOR_SIZE as u64, b"WORLD")
        .unwrap();

    let shared = SharedIsoDisk::new(Box::new(iso_disk)).unwrap();
    assert_eq!(shared.sector_count(), 2);

    let mut fw = shared.clone();
    let mut atapi = shared.clone();

    // Firmware CD trait: sector 0.
    let mut buf0 = [0u8; CDROM_SECTOR_SIZE];
    fw.read_sector(0, &mut buf0).unwrap();
    assert_eq!(&buf0[..5], b"HELLO");

    // ATAPI ISO backend trait: sector 1.
    let mut buf1 = vec![0u8; AtapiCdrom::SECTOR_SIZE];
    IsoBackend::read_sectors(&mut atapi, 1, &mut buf1).unwrap();
    assert_eq!(&buf1[..5], b"WORLD");

    // Bounds checks: both traits should report out-of-range errors.
    let mut buf_oob = [0u8; CDROM_SECTOR_SIZE];
    assert_eq!(fw.read_sector(2, &mut buf_oob), Err(DiskError::OutOfRange));

    let mut buf_oob2 = vec![0u8; AtapiCdrom::SECTOR_SIZE];
    assert!(IsoBackend::read_sectors(&mut atapi, 2, &mut buf_oob2).is_err());
}
