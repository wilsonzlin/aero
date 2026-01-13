use aero_devices_nvme::{DiskError, NvmeController, NvmePciDevice};
use aero_storage::{MemBackend, RawDisk};

#[test]
fn nvme_constructors_accept_512b_aligned_disks() {
    let disk = RawDisk::create(MemBackend::new(), 8 * 512).unwrap();
    NvmeController::try_new_from_virtual_disk(Box::new(disk)).unwrap();

    let disk = RawDisk::create(MemBackend::new(), 8 * 512).unwrap();
    NvmePciDevice::try_new_from_virtual_disk(Box::new(disk)).unwrap();
}

#[test]
fn nvme_constructors_reject_unaligned_capacity() {
    let disk = RawDisk::create(MemBackend::new(), 8 * 512 + 1).unwrap();
    assert!(matches!(
        NvmeController::try_new_from_virtual_disk(Box::new(disk)),
        Err(DiskError::Io)
    ));

    let disk = RawDisk::create(MemBackend::new(), 8 * 512 + 1).unwrap();
    assert!(matches!(
        NvmePciDevice::try_new_from_virtual_disk(Box::new(disk)),
        Err(DiskError::Io)
    ));
}
