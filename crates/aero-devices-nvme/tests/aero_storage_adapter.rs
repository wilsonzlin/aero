use aero_devices_nvme::{from_virtual_disk, DiskError};
use aero_storage::{MemBackend, RawDisk};

#[test]
fn aero_storage_adapter_read_write_roundtrip() {
    let capacity_bytes = 8u64 * 512;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut disk = from_virtual_disk(Box::new(disk));

    let payload: Vec<u8> = (0..(2 * 512)).map(|i| (i & 0xff) as u8).collect();
    disk.write_sectors(2, &payload).unwrap();

    let mut out = vec![0u8; payload.len()];
    disk.read_sectors(2, &mut out).unwrap();
    assert_eq!(out, payload);
}

#[test]
fn aero_storage_adapter_maps_out_of_range() {
    let capacity_bytes = 2u64 * 512;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut disk = from_virtual_disk(Box::new(disk));

    let mut buf = vec![0u8; 512];
    let err = disk.read_sectors(2, &mut buf).unwrap_err();
    assert_eq!(
        err,
        DiskError::OutOfRange {
            lba: 2,
            sectors: 1,
            capacity_sectors: 2
        }
    );
}

#[test]
fn aero_storage_adapter_maps_unaligned_buffer() {
    let capacity_bytes = 2u64 * 512;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut disk = from_virtual_disk(Box::new(disk));

    let mut buf = vec![0u8; 513];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert_eq!(
        err,
        DiskError::UnalignedBuffer {
            len: 513,
            sector_size: 512
        }
    );
}

#[test]
fn aero_storage_adapter_truncates_partial_trailing_sector() {
    // Capacity is not a multiple of 512; the adapter should expose only whole sectors.
    let capacity_bytes = 2u64 * 512 + 1;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut disk = from_virtual_disk(Box::new(disk));

    assert_eq!(disk.total_sectors(), 2);

    // Sector 1 (second sector) is still within the exposed capacity and should be usable.
    let payload = vec![0xA5u8; 512];
    disk.write_sectors(1, &payload).unwrap();
    let mut out = vec![0u8; 512];
    disk.read_sectors(1, &mut out).unwrap();
    assert_eq!(out, payload);

    // Sector 2 is past the exposed end and should be rejected.
    let mut out = vec![0u8; 512];
    let err = disk.read_sectors(2, &mut out).unwrap_err();
    assert_eq!(
        err,
        DiskError::OutOfRange {
            lba: 2,
            sectors: 1,
            capacity_sectors: 2
        }
    );
}
