use aero_devices_nvme::{from_virtual_disk, DiskError};
use aero_storage::{MemBackend, RawDisk};

#[test]
fn aero_storage_adapter_read_write_roundtrip() {
    let capacity_bytes = 8u64 * 512;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut disk = from_virtual_disk(Box::new(disk)).unwrap();

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
    let mut disk = from_virtual_disk(Box::new(disk)).unwrap();

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
    let mut disk = from_virtual_disk(Box::new(disk)).unwrap();

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
fn aero_storage_adapter_rejects_unaligned_capacity() {
    let capacity_bytes = 2u64 * 512 + 1;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    assert!(matches!(from_virtual_disk(Box::new(disk)), Err(DiskError::Io)));
}
