use aero_virtio::devices::blk::{MemDisk, VirtioBlk};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, VIRTIO_PCI_LEGACY_STATUS, VIRTIO_STATUS_ACKNOWLEDGE,
};

/// `VirtioPciDevice` should enforce PCI BAR decode gating internally so it behaves correctly even
/// when called directly (without an outer PCI bus/router that already checks `COMMAND.MEM`).
#[test]
fn virtio_pci_bar0_mmio_is_gated_on_pci_command_mem() {
    let blk = VirtioBlk::new(MemDisk::new(16 * 512));
    let mut dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));

    // PCI COMMAND starts cleared (decode disabled). Reads should behave like open bus.
    let mut buf = [0u8; 4];
    dev.bar0_read(0x00, &mut buf);
    assert_eq!(buf, [0xFF; 4]);

    // Writes should be ignored while MEM decoding is disabled.
    dev.bar0_write(0x14, &[1]); // common_cfg.device_status

    // Enable MEM decoding and verify the write above did not land.
    dev.config_write(0x04, &0x0002u16.to_le_bytes()); // COMMAND.MEM
    let mut status = [0u8; 1];
    dev.bar0_read(0x14, &mut status);
    assert_eq!(status[0], 0);

    // Now the write should take effect.
    dev.bar0_write(0x14, &[1]);
    dev.bar0_read(0x14, &mut status);
    assert_eq!(status[0], 1);
}

/// Like BAR0 MMIO, legacy virtio-pci I/O port accesses should be gated on `COMMAND.IO` so direct
/// callers cannot program the device before decode is enabled.
#[test]
fn virtio_pci_legacy_io_is_gated_on_pci_command_io() {
    let blk = VirtioBlk::new(MemDisk::new(16 * 512));
    let mut dev =
        VirtioPciDevice::new_transitional(Box::new(blk), Box::new(InterruptLog::default()));

    // PCI COMMAND starts cleared (decode disabled). Reads should behave like open bus.
    let mut status = [0u8; 1];
    dev.legacy_io_read(VIRTIO_PCI_LEGACY_STATUS, &mut status);
    assert_eq!(status[0], 0xFF);

    // Writes should be ignored while IO decoding is disabled.
    dev.legacy_io_write(VIRTIO_PCI_LEGACY_STATUS, &[VIRTIO_STATUS_ACKNOWLEDGE]);

    // Enable IO decoding and verify the write above did not land.
    dev.config_write(0x04, &0x0001u16.to_le_bytes()); // COMMAND.IO
    dev.legacy_io_read(VIRTIO_PCI_LEGACY_STATUS, &mut status);
    assert_eq!(status[0], 0);

    // Now the write should take effect.
    dev.legacy_io_write(VIRTIO_PCI_LEGACY_STATUS, &[VIRTIO_STATUS_ACKNOWLEDGE]);
    dev.legacy_io_read(VIRTIO_PCI_LEGACY_STATUS, &mut status);
    assert_eq!(status[0], VIRTIO_STATUS_ACKNOWLEDGE);
}
