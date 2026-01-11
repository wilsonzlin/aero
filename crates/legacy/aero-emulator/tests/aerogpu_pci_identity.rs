#[test]
fn aerogpu_pci_identity_matches_protocol() {
    use aero_emulator::devices::aerogpu::{
        AEROGPU_MMIO_BAR_SIZE, AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, AEROGPU_PCI_DEVICE_ID,
        AEROGPU_PCI_PROG_IF, AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, AEROGPU_PCI_VENDOR_ID,
    };
    use aero_protocol::aerogpu::aerogpu_pci as proto;

    assert_eq!(AEROGPU_PCI_VENDOR_ID, proto::AEROGPU_PCI_VENDOR_ID);
    assert_eq!(AEROGPU_PCI_DEVICE_ID, proto::AEROGPU_PCI_DEVICE_ID);
    assert_eq!(
        AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER,
        proto::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER
    );
    assert_eq!(
        AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE,
        proto::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE
    );
    assert_eq!(AEROGPU_PCI_PROG_IF, proto::AEROGPU_PCI_PROG_IF);
    assert_eq!(
        AEROGPU_MMIO_BAR_SIZE,
        u64::from(proto::AEROGPU_PCI_BAR0_SIZE_BYTES)
    );
}
