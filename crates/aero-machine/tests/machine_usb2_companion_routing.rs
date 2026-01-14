#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::{USB_EHCI_ICH9, USB_UHCI_PIIX3};
use aero_devices::usb::ehci::EhciPciDevice;
use aero_devices::usb::uhci::regs as uhci_regs;
use aero_machine::{Machine, MachineConfig};
use aero_usb::ehci::regs::{reg_portsc, CONFIGFLAG_CF, PORTSC_PO, PORTSC_PR, REG_CONFIGFLAG};
use aero_usb::hid::UsbHidKeyboardHandle;

#[test]
fn machine_usb2_companion_routing_moves_devices_between_uhci_and_ehci() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_ehci: true,
        // Keep the machine minimal/deterministic for this routing test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let (uhci_io_base, ehci_mmio_base) = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let uhci_bar4_base = bus
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist")
            .bar_range(4)
            .map(|range| range.base)
            .unwrap_or(0);
        let uhci_io_base = u16::try_from(uhci_bar4_base).expect("UHCI BAR4 base should fit in u16");

        let ehci_mmio_base = bus
            .device_config(USB_EHCI_ICH9.bdf)
            .expect("EHCI PCI function should exist")
            .bar_range(EhciPciDevice::MMIO_BAR_INDEX)
            .map(|range| range.base)
            .unwrap_or(0);

        (uhci_io_base, ehci_mmio_base)
    };
    assert_ne!(ehci_mmio_base, 0, "EHCI BAR0 base should be programmed");

    // Attach a full-speed USB HID keyboard to UHCI root port 0. When the machine is configured
    // with both UHCI + EHCI enabled, this root port is backed by a shared USB2 mux.
    m.usb_attach_root(0, Box::new(UsbHidKeyboardHandle::new()))
        .expect("attach should succeed");

    // Reset the UHCI view of port 0 so the device becomes enabled/routable.
    const UHCI_PORTSC_PR: u32 = 1 << 9;
    m.io_write(uhci_io_base + uhci_regs::REG_PORTSC1, 2, UHCI_PORTSC_PR);
    for _ in 0..50 {
        m.tick_platform(1_000_000);
    }

    // Before EHCI takes ownership (CONFIGFLAG=0), UHCI should be able to see the device.
    {
        let uhci = m.uhci().expect("UHCI device should exist");
        assert!(
            uhci.borrow_mut()
                .controller_mut()
                .hub_mut()
                .device_mut_for_address(0)
                .is_some(),
            "device should be reachable via UHCI while CONFIGFLAG=0"
        );
    }
    {
        let ehci = m.ehci().expect("EHCI device should exist");
        assert!(
            ehci.borrow_mut()
                .controller_mut()
                .hub_mut()
                .device_mut_for_address(0)
                .is_none(),
            "device should not be reachable via EHCI while CONFIGFLAG=0"
        );
    }

    // Let the guest claim ports for EHCI.
    m.write_physical_u32(ehci_mmio_base + REG_CONFIGFLAG, CONFIGFLAG_CF);
    let portsc = m.read_physical_u32(ehci_mmio_base + reg_portsc(0));
    assert_eq!(portsc & PORTSC_PO, 0, "CONFIGFLAG should clear PORT_OWNER");

    // Reset the EHCI view of the shared port and ensure the device becomes reachable.
    m.write_physical_u32(ehci_mmio_base + reg_portsc(0), PORTSC_PR);
    for _ in 0..50 {
        m.tick_platform(1_000_000);
    }

    {
        let uhci = m.uhci().expect("UHCI device should exist");
        assert!(
            uhci.borrow_mut()
                .controller_mut()
                .hub_mut()
                .device_mut_for_address(0)
                .is_none(),
            "device should no longer be reachable via UHCI after CONFIGFLAG"
        );
    }
    {
        let ehci = m.ehci().expect("EHCI device should exist");
        assert!(
            ehci.borrow_mut()
                .controller_mut()
                .hub_mut()
                .device_mut_for_address(0)
                .is_some(),
            "device should be reachable via EHCI after CONFIGFLAG + port reset"
        );
    }
}
