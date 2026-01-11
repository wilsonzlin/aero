use aero_protocol::aerogpu::aerogpu_pci as proto;
use emulator::devices::aerogpu_regs as emu;

#[test]
fn aerogpu_abi_constants_match_aero_protocol() {
    // PCI IDs.
    assert_eq!(emu::AEROGPU_PCI_VENDOR_ID, proto::AEROGPU_PCI_VENDOR_ID);
    assert_eq!(emu::AEROGPU_PCI_DEVICE_ID, proto::AEROGPU_PCI_DEVICE_ID);
    assert_eq!(
        emu::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
        proto::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID
    );
    assert_eq!(emu::AEROGPU_PCI_SUBSYSTEM_ID, proto::AEROGPU_PCI_SUBSYSTEM_ID);

    // BAR sizing.
    assert_eq!(
        emu::AEROGPU_PCI_BAR0_SIZE_BYTES,
        proto::AEROGPU_PCI_BAR0_SIZE_BYTES as u64
    );

    // ABI + identity.
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
    assert_eq!(emu::FEATURE_SCANOUT, proto::AEROGPU_FEATURE_SCANOUT);
    assert_eq!(emu::FEATURE_VBLANK, proto::AEROGPU_FEATURE_VBLANK);
}
