use emulator::io::usb::core::{UsbInResult, UsbOutResult};
use emulator::io::usb::hub::RootHub;
use emulator::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel};

struct TestUsbDevice;

impl UsbDeviceModel for TestUsbDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        None
    }
}

#[test]
fn uhci_portsc_port_reset_is_bit9_and_resets_device_state() {
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_LS_MASK: u16 = 0b11 << 4;
    const PORTSC_LS_J_FS: u16 = 0b01 << 4;
    const PORTSC_LSDA: u16 = 1 << 8;
    const PORTSC_PR: u16 = 1 << 9;

    let mut hub = RootHub::new();
    hub.attach(0, Box::new(TestUsbDevice));
    hub.force_enable_for_tests(0);

    // Give the device a non-zero address so we can observe that a port reset
    // returns the device to the default-address state.
    {
        let dev = hub
            .device_mut_for_address(0)
            .expect("device should be visible at address 0");

        let setup = SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 5,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
        assert!(matches!(dev.handle_in(0, 0), UsbInResult::Data(d) if d.is_empty()));
        assert_eq!(dev.address(), 5);
    }
    assert!(hub.device_mut_for_address(5).is_some());
    assert_eq!(hub.read_portsc(0) & PORTSC_LS_MASK, PORTSC_LS_J_FS);

    // Trigger a port reset. The UHCI PORTSC PR bit is bit 9; bit 8 is LSDA.
    hub.write_portsc(0, PORTSC_PR | PORTSC_PED);

    // During reset the PR bit should read back as set and port enable should be cleared.
    let portsc = hub.read_portsc(0);
    assert_ne!(portsc & PORTSC_PR, 0);
    assert_eq!(portsc & PORTSC_PED, 0);
    assert_eq!(portsc & PORTSC_LSDA, 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0);

    // Attempts to re-enable the port while PR is still set should be ignored.
    hub.write_portsc(0, PORTSC_PED);
    let portsc = hub.read_portsc(0);
    assert_ne!(portsc & PORTSC_PR, 0);
    assert_eq!(portsc & PORTSC_PED, 0);

    for _ in 0..49 {
        hub.tick_1ms();
        let portsc = hub.read_portsc(0);
        assert_ne!(portsc & PORTSC_PR, 0);
        assert_eq!(portsc & PORTSC_PED, 0);
        assert_eq!(portsc & PORTSC_LS_MASK, 0);
    }

    // After 50ms the reset auto-completes, PR clears, and the port is enabled again.
    hub.tick_1ms();
    let portsc = hub.read_portsc(0);
    assert_eq!(portsc & PORTSC_PR, 0);
    assert_ne!(portsc & PORTSC_PED, 0);
    assert_eq!(portsc & PORTSC_LS_MASK, PORTSC_LS_J_FS);

    // The device should have returned to address 0 after the bus reset.
    assert!(hub.device_mut_for_address(5).is_none());
    let dev = hub
        .device_mut_for_address(0)
        .expect("device should be visible at address 0 after reset");
    assert_eq!(dev.address(), 0);
}

#[test]
fn uhci_portsc_connect_event_disables_previously_enabled_port() {
    const PORTSC_CCS: u16 = 1 << 0;
    const PORTSC_CSC: u16 = 1 << 1;
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_PEDC: u16 = 1 << 3;

    let mut hub = RootHub::new();
    hub.attach(0, Box::new(TestUsbDevice));

    // Clear initial connection-change.
    hub.write_portsc(0, PORTSC_CSC);

    // Enable the port and clear its enable-change bit.
    hub.write_portsc(0, PORTSC_PED);
    hub.write_portsc(0, PORTSC_PED | PORTSC_PEDC);

    let st = hub.read_portsc(0);
    assert_eq!(st & (PORTSC_CCS | PORTSC_CSC), PORTSC_CCS);
    assert_eq!(st & (PORTSC_PED | PORTSC_PEDC), PORTSC_PED);

    // Attaching a new device while the port was enabled should clear PED and set PEDC/CSC.
    hub.attach(0, Box::new(TestUsbDevice));
    let st = hub.read_portsc(0);
    assert_eq!(st & PORTSC_CCS, PORTSC_CCS);
    assert_eq!(st & PORTSC_CSC, PORTSC_CSC);
    assert_eq!(st & PORTSC_PED, 0);
    assert_eq!(st & PORTSC_PEDC, PORTSC_PEDC);

    // Port is disabled, so the device should no longer be routable.
    assert!(hub.device_mut_for_address(0).is_none());
}
