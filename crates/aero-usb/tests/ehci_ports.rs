use aero_usb::ehci::regs::{
    reg_portsc, CONFIGFLAG_CF, PORTSC_CSC, PORTSC_PED, PORTSC_PEDC, PORTSC_PO, REG_CONFIGFLAG,
    REG_USBSTS, USBSTS_PCD,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;

#[test]
fn ehci_configflag_routes_ports_and_owner_writes_trigger_pcd() {
    let mut ehci = EhciController::new_with_port_count(2);

    // Attach a full-speed device.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut().attach(0, Box::new(keyboard));

    // By default ports are owned by a (non-existent) companion controller.
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc0 & PORTSC_PO, 0);

    // Clear connect status change so PCD can be cleared deterministically.
    if portsc0 & PORTSC_CSC != 0 {
        // W1C: write 1 to clear, preserving other bits.
        ehci.mmio_write(reg_portsc(0), 4, portsc0 | PORTSC_CSC);
    }

    // Clear any lingering PCD (write-1-to-clear).
    ehci.mmio_write(REG_USBSTS, 4, USBSTS_PCD);
    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_eq!(st & USBSTS_PCD, 0);

    // Attempt to enable the port while PORT_OWNER=1: enable/reset should be ignored.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PED);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & PORTSC_PED, 0);

    // Device should not be reachable while PORT_OWNER=1.
    assert!(ehci.hub_mut().device_mut_for_address(0).is_none());

    // CONFIGFLAG 0->1 should route ports to EHCI ownership, clearing PORT_OWNER and asserting PCD.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc0 & PORTSC_PO, 0);

    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_ne!(st & USBSTS_PCD, 0);

    // Enable the port now that EHCI owns it.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PED);

    // The device should now be reachable by the EHCI controller (enabled + owned).
    assert!(ehci.hub_mut().device_mut_for_address(0).is_some());

    // Clear PCD again to test direct PORT_OWNER writes. Since enabling the port set PEDC, clear it.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    if portsc & PORTSC_PEDC != 0 {
        ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PEDC);
    }
    ehci.mmio_write(REG_USBSTS, 4, USBSTS_PCD);
    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_eq!(st & USBSTS_PCD, 0);

    // PORT_OWNER is only writable when the port is disabled. Disable it first (preserving power).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc & !PORTSC_PED);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & PORTSC_PED, 0);

    // Clear the resulting PEDC so PCD can be isolated to the owner change.
    if portsc & PORTSC_PEDC != 0 {
        ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PEDC);
    }
    ehci.mmio_write(REG_USBSTS, 4, USBSTS_PCD);
    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_eq!(st & USBSTS_PCD, 0);

    // Now set PORT_OWNER=1 and ensure PCD latches.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PO);
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc0 & PORTSC_PO, 0);

    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_ne!(st & USBSTS_PCD, 0);

    // With PORT_OWNER=1 and no companion present, the device is unreachable.
    assert!(ehci.hub_mut().device_mut_for_address(0).is_none());
}
