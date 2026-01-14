use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{UsbSpeed, UsbWebUsbPassthroughDevice};

// EHCI PORTSC bits we care about.
const EHCI_PORT_CCS: u32 = 1 << 0;
const EHCI_PORT_FPR: u32 = 1 << 6;
const EHCI_PORT_SUSP: u32 = 1 << 7;
const EHCI_PORT_HSP: u32 = 1 << 9;
const EHCI_PORT_LS_MASK: u32 = 0b11 << 10;
const EHCI_PORT_OWNER: u32 = 1 << 13;
const EHCI_PORT_FPR: u32 = 1 << 6;
const EHCI_PORT_SUSP: u32 = 1 << 7;
const EHCI_PORT_PR: u32 = 1 << 8;

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

    // During resume at low/full speed, the host should report a K-state (D- high) via the LS bits.
    mux.ehci_write_portsc_masked(0, EHCI_PORT_SUSP, EHCI_PORT_SUSP);
    let portsc = mux.ehci_read_portsc(0);
    assert_ne!(portsc & EHCI_PORT_SUSP, 0, "expected SUSP set after suspend request");

    mux.ehci_write_portsc_masked(0, EHCI_PORT_FPR, EHCI_PORT_FPR);
    let portsc = mux.ehci_read_portsc(0);
    assert_ne!(portsc & EHCI_PORT_FPR, 0, "expected FPR set while resuming");
    assert_eq!(
        (portsc & EHCI_PORT_LS_MASK) >> 10,
        0b01,
        "expected K-state while resuming"
    );

    for _ in 0..20 {
        mux.ehci_tick_1ms(0);
    }
    let portsc = mux.ehci_read_portsc(0);
    assert_eq!(portsc & EHCI_PORT_FPR, 0, "expected resume to complete");
    assert_eq!(
        (portsc & EHCI_PORT_LS_MASK) >> 10,
        0b10,
        "expected J-state after resume completes"
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

#[test]
fn usb2_port_mux_ehci_line_status_flips_to_k_state_during_resume_for_full_speed() {
    let mut mux = Usb2PortMux::new(1);
    mux.attach(
        0,
        Box::new(UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::Full)),
    );

    // Claim the port for EHCI (CONFIGFLAG=1 + PORT_OWNER=0).
    mux.set_configflag(true);
    mux.ehci_write_portsc_masked(0, 0, EHCI_PORT_OWNER);

    // Reset to enable.
    mux.ehci_write_portsc_masked(0, EHCI_PORT_PR, EHCI_PORT_PR);
    for _ in 0..50 {
        mux.ehci_tick_1ms(0);
    }

    let portsc = mux.ehci_read_portsc(0);
    assert_eq!(portsc & EHCI_PORT_HSP, 0);
    assert_eq!(portsc & EHCI_PORT_LS_MASK, 0b10 << 10, "expected J-state when idle");

    // Suspend, then force resume and verify LS reports K-state while resuming.
    mux.ehci_write_portsc_masked(0, EHCI_PORT_SUSP, EHCI_PORT_SUSP);
    mux.ehci_write_portsc_masked(0, EHCI_PORT_FPR, EHCI_PORT_FPR);
    let portsc = mux.ehci_read_portsc(0);
    assert_ne!(portsc & EHCI_PORT_FPR, 0, "expected FPR set while resuming");
    assert_eq!(portsc & EHCI_PORT_LS_MASK, 0b01 << 10, "expected K-state while resuming");

    // After the resume timer expires, the port should return to J state.
    for _ in 0..20 {
        mux.ehci_tick_1ms(0);
    }
    let portsc = mux.ehci_read_portsc(0);
    assert_eq!(portsc & EHCI_PORT_FPR, 0);
    assert_eq!(portsc & EHCI_PORT_SUSP, 0);
    assert_eq!(portsc & EHCI_PORT_LS_MASK, 0b10 << 10);
}
