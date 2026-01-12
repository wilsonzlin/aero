use aero_protocol::aerogpu::aerogpu_pci as proto;
use emulator::devices::aerogpu_regs as emu;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::io::pci::PciDevice;

fn read_u8(dev: &dyn PciDevice, offset: u16) -> u8 {
    dev.config_read(offset, 1) as u8
}

fn read_u16(dev: &dyn PciDevice, offset: u16) -> u16 {
    dev.config_read(offset, 2) as u16
}

#[test]
fn aerogpu_abi_constants_match_aero_protocol() {
    // PCI IDs.
    assert_eq!(emu::AEROGPU_PCI_VENDOR_ID, proto::AEROGPU_PCI_VENDOR_ID);
    assert_eq!(emu::AEROGPU_PCI_DEVICE_ID, proto::AEROGPU_PCI_DEVICE_ID);
    assert_eq!(
        emu::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
        proto::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID
    );
    assert_eq!(
        emu::AEROGPU_PCI_SUBSYSTEM_ID,
        proto::AEROGPU_PCI_SUBSYSTEM_ID
    );

    // PCI class identity.
    assert_eq!(
        emu::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER,
        proto::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER
    );
    assert_eq!(
        emu::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE,
        proto::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE
    );
    assert_eq!(emu::AEROGPU_PCI_PROG_IF, proto::AEROGPU_PCI_PROG_IF);

    // BAR sizing.
    assert_eq!(
        emu::AEROGPU_PCI_BAR0_SIZE_BYTES,
        proto::AEROGPU_PCI_BAR0_SIZE_BYTES as u64
    );

    // ABI + identity.
    assert_eq!(emu::AEROGPU_ABI_MAJOR, proto::AEROGPU_ABI_MAJOR);
    assert_eq!(emu::AEROGPU_ABI_MINOR, proto::AEROGPU_ABI_MINOR);
    assert_eq!(emu::AEROGPU_ABI_VERSION_U32, proto::AEROGPU_ABI_VERSION_U32);
    assert_eq!(emu::AEROGPU_MMIO_MAGIC, proto::AEROGPU_MMIO_MAGIC);

    // MMIO register map (subset).
    assert_eq!(emu::mmio::MAGIC, proto::AEROGPU_MMIO_REG_MAGIC as u64);
    assert_eq!(
        emu::mmio::ABI_VERSION,
        proto::AEROGPU_MMIO_REG_ABI_VERSION as u64
    );
    assert_eq!(emu::mmio::DOORBELL, proto::AEROGPU_MMIO_REG_DOORBELL as u64);
    assert_eq!(
        emu::mmio::IRQ_STATUS,
        proto::AEROGPU_MMIO_REG_IRQ_STATUS as u64
    );
    assert_eq!(
        emu::mmio::IRQ_ENABLE,
        proto::AEROGPU_MMIO_REG_IRQ_ENABLE as u64
    );
    assert_eq!(emu::mmio::IRQ_ACK, proto::AEROGPU_MMIO_REG_IRQ_ACK as u64);

    assert_eq!(
        emu::mmio::SCANOUT0_ENABLE,
        proto::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64
    );
    assert_eq!(
        emu::mmio::SCANOUT0_WIDTH,
        proto::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64
    );
    assert_eq!(
        emu::mmio::SCANOUT0_HEIGHT,
        proto::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64
    );
    assert_eq!(
        emu::mmio::SCANOUT0_FORMAT,
        proto::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64
    );
    assert_eq!(
        emu::mmio::SCANOUT0_PITCH_BYTES,
        proto::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64
    );
    assert_eq!(
        emu::mmio::SCANOUT0_FB_GPA_LO,
        proto::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64
    );
    assert_eq!(
        emu::mmio::SCANOUT0_FB_GPA_HI,
        proto::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64
    );

    // Feature bits (subset).
    assert_eq!(emu::FEATURE_FENCE_PAGE, proto::AEROGPU_FEATURE_FENCE_PAGE);
    assert_eq!(emu::FEATURE_CURSOR, proto::AEROGPU_FEATURE_CURSOR);
    assert_eq!(emu::FEATURE_TRANSFER, proto::AEROGPU_FEATURE_TRANSFER);
    assert_eq!(emu::FEATURE_SCANOUT, proto::AEROGPU_FEATURE_SCANOUT);
    assert_eq!(emu::FEATURE_VBLANK, proto::AEROGPU_FEATURE_VBLANK);
}

#[test]
fn aerogpu_pci_bar0_is_masked_to_bar_size_alignment() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    dev.config_write(0x10, 4, 0xffff_ffff);
    let mask = dev.config_read(0x10, 4);
    assert_eq!(
        mask,
        (!(emu::AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1)) & 0xffff_fff0
    );

    dev.config_write(0x10, 4, 0xdead_beef);
    assert_eq!(dev.config_read(0x10, 4), 0xdead_beef & mask);
    assert_eq!(dev.bar0, 0xdead_beef & mask);
}

#[test]
fn aerogpu_pci_config_space_uses_protocol_identity() {
    let dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    assert_eq!(read_u16(&dev, 0x00), proto::AEROGPU_PCI_VENDOR_ID);
    assert_eq!(read_u16(&dev, 0x02), proto::AEROGPU_PCI_DEVICE_ID);

    assert_eq!(read_u16(&dev, 0x2c), proto::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID);
    assert_eq!(read_u16(&dev, 0x2e), proto::AEROGPU_PCI_SUBSYSTEM_ID);

    assert_eq!(read_u8(&dev, 0x09), proto::AEROGPU_PCI_PROG_IF);
    assert_eq!(
        read_u8(&dev, 0x0a),
        proto::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE
    );
    assert_eq!(
        read_u8(&dev, 0x0b),
        proto::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER
    );
}
