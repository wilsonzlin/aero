use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{UsbSpeed, UsbWebUsbPassthroughDevice};

// EHCI PORTSC bits we care about.
const EHCI_PORT_CCS: u32 = 1 << 0;
const EHCI_PORT_HSP: u32 = 1 << 9;
const EHCI_PORT_LS_MASK: u32 = 0b11 << 10;
const EHCI_PORT_OWNER: u32 = 1 << 13;

#[test]
fn usb2_port_mux_ehci_portsc_reports_device_speed() {
    let mut mux = Usb2PortMux::new(1);

    mux.attach(
        0,
        Box::new(UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High)),
    );

    // Even before EHCI claims the port, it should still be able to see a physical connection while
    // PORT_OWNER=1 so guests can observe attach state and decide whether to take ownership.
    let portsc = mux.ehci_read_portsc(0);
    assert_ne!(
        portsc & EHCI_PORT_OWNER,
        0,
        "expected port to start companion-owned"
    );
    assert_ne!(
        portsc & EHCI_PORT_CCS,
        0,
        "expected CCS set when device is attached"
    );
    assert_eq!(
        portsc & EHCI_PORT_HSP,
        0,
        "HSP should be clear when the port is companion-owned"
    );

    // Claim the port for EHCI (CONFIGFLAG=1 + PORT_OWNER=0).
    mux.set_configflag(true);
    mux.ehci_write_portsc_masked(0, 0, EHCI_PORT_OWNER);

    let portsc = mux.ehci_read_portsc(0);
    assert_ne!(
        portsc & EHCI_PORT_HSP,
        0,
        "high-speed device should set HSP"
    );
    assert_eq!(
        portsc & EHCI_PORT_LS_MASK,
        0,
        "high-speed device should clear LS bits"
    );

    mux.detach(0);
    mux.attach(
        0,
        Box::new(UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::Full)),
    );

    let portsc = mux.ehci_read_portsc(0);
    assert_eq!(
        portsc & EHCI_PORT_HSP,
        0,
        "full-speed device should clear HSP"
    );
    assert_eq!(
        (portsc & EHCI_PORT_LS_MASK) >> 10,
        0b10,
        "full-speed device should report idle J-state via LS"
    );

    mux.detach(0);
    mux.attach(
        0,
        Box::new(UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::Low)),
    );

    let portsc = mux.ehci_read_portsc(0);
    assert_eq!(
        portsc & EHCI_PORT_HSP,
        0,
        "low-speed device should clear HSP"
    );
    assert_eq!(
        (portsc & EHCI_PORT_LS_MASK) >> 10,
        0b01,
        "low-speed device should report idle K-state via LS"
    );
}
