use aero_devices::pci::{PciBarDefinition, PciDevice};
use aero_devices::usb::xhci::XhciPciDevice;
use aero_usb::xhci::regs;
use memory::MmioHandler;

#[test]
fn xhci_pci_mmio_bar_definition_is_exposed_on_bar0() {
    let mut dev = XhciPciDevice::default();
    let cfg = dev.config_mut();

    assert_eq!(
        cfg.bar_definition(XhciPciDevice::MMIO_BAR_INDEX),
        Some(PciBarDefinition::Mmio32 {
            size: u32::try_from(XhciPciDevice::MMIO_BAR_SIZE)
                .expect("xHCI BAR size should fit in u32"),
            prefetchable: false
        })
    );
}

#[test]
fn xhci_pci_mmio_bar_size_probe_returns_expected_mask() {
    let mut dev = XhciPciDevice::default();
    let cfg = dev.config_mut();

    let bar_offset = 0x10 + u16::from(XhciPciDevice::MMIO_BAR_INDEX) * 4;

    // Standard PCI BAR size probing: write all 1s, then read back the size mask.
    cfg.write(bar_offset, 4, 0xFFFF_FFFF);

    let size = u32::try_from(XhciPciDevice::MMIO_BAR_SIZE).expect("xHCI BAR size should fit in u32");
    let expected_mask = !(size.saturating_sub(1)) & 0xFFFF_FFF0;
    assert_eq!(cfg.read(bar_offset, 4), expected_mask);
}

#[test]
fn xhci_mmio_reads_float_high_when_command_mem_is_disabled() {
    let mut dev = XhciPciDevice::default();

    // COMMAND.MEM (bit 1) disabled: reads should float high and writes should be ignored.
    dev.config_mut().set_command(0);
    assert_eq!(
        MmioHandler::read(&mut dev, regs::REG_CAPLENGTH_HCIVERSION, 4),
        0xFFFF_FFFF
    );

    MmioHandler::write(
        &mut dev,
        regs::REG_USBCMD,
        4,
        u64::from(regs::USBCMD_RUN),
    );

    // Enable memory decoding: MMIO reads should now reach the controller implementation.
    dev.config_mut().set_command(1 << 1);

    let cap = MmioHandler::read(&mut dev, regs::REG_CAPLENGTH_HCIVERSION, 4);
    assert_ne!(
        cap, 0xFFFF_FFFF,
        "xHCI capability registers should be visible when COMMAND.MEM is set"
    );

    // The earlier write must have been ignored.
    assert_eq!(
        MmioHandler::read(&mut dev, regs::REG_USBCMD, 4) & u64::from(regs::USBCMD_RUN),
        0
    );

    MmioHandler::write(
        &mut dev,
        regs::REG_USBCMD,
        4,
        u64::from(regs::USBCMD_RUN),
    );
    assert_ne!(
        MmioHandler::read(&mut dev, regs::REG_USBCMD, 4) & u64::from(regs::USBCMD_RUN),
        0
    );
}

