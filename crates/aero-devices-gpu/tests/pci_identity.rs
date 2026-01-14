use aero_devices::pci::PciDevice;
use aero_devices_gpu::AeroGpuPciDevice;
use aero_protocol::aerogpu::aerogpu_pci as proto;

#[test]
fn aerogpu_pci_config_space_matches_protocol_identity() {
    let mut dev = AeroGpuPciDevice::default();
    let cfg = dev.config_mut();

    let ids = cfg.vendor_device_id();
    assert_eq!(ids.vendor_id, proto::AEROGPU_PCI_VENDOR_ID);
    assert_eq!(ids.device_id, proto::AEROGPU_PCI_DEVICE_ID);

    let class = cfg.class_code();
    assert_eq!(
        class.class,
        proto::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER
    );
    assert_eq!(class.subclass, proto::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE);
    assert_eq!(class.prog_if, proto::AEROGPU_PCI_PROG_IF);

    // The config-space snapshot uses the same raw little-endian encoding as PCI.
    assert_eq!(
        cfg.read(0x2c, 2) as u16,
        proto::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID
    );
    assert_eq!(
        cfg.read(0x2e, 2) as u16,
        proto::AEROGPU_PCI_SUBSYSTEM_ID
    );

    let bar0 = cfg
        .bar_range(proto::AEROGPU_PCI_BAR0_INDEX as u8)
        .expect("AeroGPU BAR0 must exist");
    assert_eq!(bar0.size, u64::from(proto::AEROGPU_PCI_BAR0_SIZE_BYTES));
}
