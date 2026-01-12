use aero_devices_nvme::{from_virtual_disk, DiskError};
use aero_storage::{MemBackend, RawDisk};
use aero_storage::VirtualDisk;

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

#[test]
fn aero_storage_adapter_maps_underlying_disk_error_to_io() {
    struct FaultyDisk {
        capacity_bytes: u64,
    }

    impl VirtualDisk for FaultyDisk {
        fn capacity_bytes(&self) -> u64 {
            self.capacity_bytes
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity_bytes,
            })
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity_bytes,
            })
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::Io("forced flush failure".to_string()))
        }
    }

    let disk = FaultyDisk { capacity_bytes: 512 };
    let mut disk = from_virtual_disk(Box::new(disk));

    let mut buf = vec![0u8; 512];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert_eq!(err, DiskError::Io);

    let payload = vec![0xAAu8; 512];
    let err = disk.write_sectors(0, &payload).unwrap_err();
    assert_eq!(err, DiskError::Io);

    let err = disk.flush().unwrap_err();
    assert_eq!(err, DiskError::Io);
}
