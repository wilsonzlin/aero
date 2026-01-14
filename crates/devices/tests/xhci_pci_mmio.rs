use aero_devices::pci::profile::{
    PCI_DEVICE_ID_QEMU_XHCI, PCI_VENDOR_ID_REDHAT_QEMU, XHCI_MMIO_BAR_SIZE,
};
use aero_devices::pci::{PciBarDefinition, PciDevice};
use aero_devices::usb::xhci::{regs, XhciPciDevice};
use memory::MmioHandler;

#[test]
fn xhci_bar0_mmio_definition_and_probe_mask() {
    let mut dev = XhciPciDevice::default();

    // Config-space identity should match the canonical QEMU xHCI profile.
    let id = dev.config().vendor_device_id();
    assert_eq!(id.vendor_id, PCI_VENDOR_ID_REDHAT_QEMU);
    assert_eq!(id.device_id, PCI_DEVICE_ID_QEMU_XHCI);

    let class = dev.config().class_code();
    assert_eq!(class.class, 0x0c);
    assert_eq!(class.subclass, 0x03);
    assert_eq!(class.prog_if, 0x30);

    assert_eq!(
        dev.config().bar_definition(XhciPciDevice::MMIO_BAR_INDEX),
        Some(PciBarDefinition::Mmio32 {
            size: XHCI_MMIO_BAR_SIZE as u32,
            prefetchable: false
        })
    );

    // Standard PCI BAR size probing: write all 1s, then read back the size mask.
    let bar0_cfg_offset = 0x10u16 + u16::from(XhciPciDevice::MMIO_BAR_INDEX) * 4;
    dev.config_mut().write(bar0_cfg_offset, 4, 0xffff_ffff);
    assert_eq!(
        dev.config_mut().read(bar0_cfg_offset, 4),
        (!(XHCI_MMIO_BAR_SIZE as u32 - 1) & 0xffff_fff0),
        "BAR0 size probe mismatch"
    );
}

#[test]
fn xhci_mmio_reads_capability_regs_when_mem_enabled() {
    let mut dev = XhciPciDevice::default();

    // Enable MMIO decoding via PCI COMMAND.MEM (bit 1).
    dev.config_mut().set_command(1 << 1);

    let cap = MmioHandler::read(&mut dev, regs::REG_CAPLENGTH_HCIVERSION, 4) as u32;
    assert_eq!(cap, regs::CAPLENGTH_HCIVERSION);

    // Reads outside the BAR window should float high even when MEM decoding is enabled.
    let oob = MmioHandler::read(&mut dev, u64::from(XhciPciDevice::MMIO_BAR_SIZE), 4);
    assert_eq!(oob, u64::from(u32::MAX));
}

#[test]
fn xhci_mmio_writes_are_ignored_when_mem_disabled() {
    let mut dev = XhciPciDevice::default();

    // COMMAND.MEM is clear by default: writes must be ignored.
    MmioHandler::write(&mut dev, regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Enable MMIO decoding and verify the earlier write did not take effect.
    dev.config_mut().set_command(1 << 1);
    let cmd = MmioHandler::read(&mut dev, regs::REG_USBCMD, 4) as u32;
    assert_eq!(cmd & regs::USBCMD_RUN, 0);

    // Writes should apply once MEM is enabled.
    MmioHandler::write(&mut dev, regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    let cmd = MmioHandler::read(&mut dev, regs::REG_USBCMD, 4) as u32;
    assert_ne!(cmd & regs::USBCMD_RUN, 0);
}
