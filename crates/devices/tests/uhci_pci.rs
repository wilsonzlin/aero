use aero_devices::pci::{PciBarDefinition, PciDevice};
use aero_devices::usb::uhci::{register_uhci_io_ports, regs, SharedUhciPciDevice, UhciPciDevice};
use aero_platform::io::IoPortBus;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn uhci_pci_config_and_bar_io() {
    let uhci: SharedUhciPciDevice = Rc::new(RefCell::new(UhciPciDevice::default()));

    // Validate config-space identity and BAR definition.
    {
        let mut dev = uhci.borrow_mut();
        let cfg = dev.config_mut();

        let id = cfg.vendor_device_id();
        assert_eq!(id.vendor_id, 0x8086);
        assert_eq!(id.device_id, 0x7020);

        let class = cfg.class_code();
        assert_eq!(class.class, 0x0c);
        assert_eq!(class.subclass, 0x03);
        assert_eq!(class.prog_if, 0x00);

        assert_eq!(
            cfg.bar_definition(UhciPciDevice::IO_BAR_INDEX),
            Some(PciBarDefinition::Io {
                size: u32::from(UhciPciDevice::IO_BAR_SIZE)
            })
        );

        // Interrupt pin/line should reflect a typical PIIX3 UHCI wiring (INTA#/IRQ11).
        assert_eq!(cfg.read(0x3d, 1) as u8, 1);
        assert_eq!(cfg.read(0x3c, 1) as u8, 11);

        // Program BAR4 and enable I/O decoding.
        cfg.set_bar_base(UhciPciDevice::IO_BAR_INDEX, 0x1000);
        cfg.set_command(0x0001);
    }

    let mut io = IoPortBus::new();
    register_uhci_io_ports(&mut io, uhci.clone());

    let base = 0x1000;

    // Default SOFMOD is 64.
    assert_eq!(io.read(base + regs::REG_SOFMOD, 1) as u8, 64);

    // Writes to the UHCI I/O window must reach the underlying controller model.
    io.write(base + regs::REG_SOFMOD, 1, 12);
    assert_eq!(io.read(base + regs::REG_SOFMOD, 1) as u8, 12);

    io.write(base + regs::REG_USBINTR, 2, regs::USBINTR_IOC as u32);
    assert_eq!(
        io.read(base + regs::REG_USBINTR, 2) as u16,
        regs::USBINTR_IOC
    );

    // Confirm the controller's state changed (not just the I/O readback path).
    assert_eq!(uhci.borrow().controller().regs().usbintr, regs::USBINTR_IOC);
}
