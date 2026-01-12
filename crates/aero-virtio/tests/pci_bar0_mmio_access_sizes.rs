use aero_memory::MmioHandler;
use aero_virtio::devices::blk::{MemDisk, VirtioBlk};
use aero_virtio::memory::GuestRam;
use aero_virtio::mmio::VirtioBar0Mmio;
use aero_virtio::pci::{InterruptLog, VirtioPciDevice, VIRTIO_STATUS_ACKNOWLEDGE};

#[test]
fn virtio_bar0_mmio_supports_common_cfg_access_sizes() {
    let blk = VirtioBlk::new(MemDisk::new(1024 * 1024));
    let pci = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));
    let mem = GuestRam::new(64 * 1024);

    let mut bar0 = VirtioBar0Mmio::new(pci, mem);

    // 1-byte access: device_status (offset 0x14)
    assert_eq!(bar0.read(0x14, 1) as u8, 0);
    bar0.write(0x14, 1, u64::from(VIRTIO_STATUS_ACKNOWLEDGE));
    assert_eq!(bar0.read(0x14, 1) as u8, VIRTIO_STATUS_ACKNOWLEDGE);

    // 2-byte access: queue_select (offset 0x16)
    bar0.write(0x16, 2, 0x1234);
    assert_eq!(bar0.read(0x16, 2) as u16, 0x1234);

    // Restore queue_select so subsequent queue field accesses operate on a valid queue.
    bar0.write(0x16, 2, 0);

    // 4-byte access: device_feature_select (offset 0x00)
    bar0.write(0x00, 4, 1);
    assert_eq!(bar0.read(0x00, 4) as u32, 1);

    // 64-bit common-cfg field: desc_addr supports split 32-bit writes and 64-bit reads.
    let desc_addr = 0x1122_3344_5566_7788u64;
    bar0.write(0x20, 4, desc_addr as u32 as u64);
    bar0.write(0x24, 4, (desc_addr >> 32) as u32 as u64);
    assert_eq!(bar0.read(0x20, 8), desc_addr);

    // 64-bit write path too.
    let used_addr = 0x8877_6655_4433_2211u64;
    bar0.write(0x30, 8, used_addr);
    assert_eq!(bar0.read(0x30, 8), used_addr);
}
