#[test]
fn aerogpu_pci_identity_is_canonical() {
    // AeroGPU's PCI IDs are part of the Windows driver contract. Even though `aero-emulator`
    // doesn't implement the AeroGPU device model anymore, keep a lightweight assertion here so
    // placeholder values (e.g. early `VEN_1AE0`/`DEV_E001` experiments) don't creep back in.
    use aero_protocol::aerogpu::aerogpu_pci as proto;

    assert_eq!(proto::AEROGPU_PCI_VENDOR_ID, 0xA3A0);
    assert_eq!(proto::AEROGPU_PCI_DEVICE_ID, 0x0001);
    assert_eq!(proto::AEROGPU_PCI_BAR0_SIZE_BYTES, 64 * 1024);
}

