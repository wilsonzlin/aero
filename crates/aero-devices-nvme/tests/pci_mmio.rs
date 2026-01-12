use aero_devices::pci::PciDevice as _;
use aero_devices_nvme::NvmePciDevice;
use memory::MmioHandler as _;

const NVME_CAP: u64 = 0x0000;
const NVME_CC: u64 = 0x0014;

#[test]
fn bar0_mmio_requires_pci_memory_space_enable() {
    let mut dev = NvmePciDevice::default();

    // Memory Space Enable (command bit 1) gates MMIO decoding: reads float high and writes are
    // ignored.
    dev.config_mut().set_command(0);
    assert_eq!(dev.read(NVME_CAP, 4), 0xFFFF_FFFF);
    assert_eq!(dev.read(NVME_CAP, 8), u64::MAX);

    // Try to enable the controller while MMIO decoding is disabled; this write should not take
    // effect.
    dev.write(NVME_CC, 4, 1);

    // Enable MMIO decoding and observe real register values again.
    dev.config_mut().set_command(0x0002); // MEM
    assert_ne!(dev.read(NVME_CAP, 4), 0xFFFF_FFFF);

    // CC.EN should still be clear because the earlier write was ignored.
    let cc = dev.read(NVME_CC, 4) as u32;
    assert_eq!(cc & 1, 0);

    // With MEM decoding enabled, writes should take effect again.
    dev.write(NVME_CC, 4, 1);
    let cc = dev.read(NVME_CC, 4) as u32;
    assert_eq!(cc & 1, 1);
}

