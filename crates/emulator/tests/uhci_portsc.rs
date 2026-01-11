use emulator::io::usb::core::{UsbInResult, UsbOutResult};
use emulator::io::usb::hub::RootHub;
use emulator::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel};

struct TestUsbDevice;

impl UsbDeviceModel for TestUsbDevice {
    fn get_device_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_config_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        &[]
    }

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

    // Trigger a port reset. The UHCI PORTSC PR bit is bit 9; bit 8 is LSDA.
    hub.write_portsc(0, PORTSC_PR | PORTSC_PED);

    // During reset the PR bit should read back as set and port enable should be cleared.
    let portsc = hub.read_portsc(0);
    assert_ne!(portsc & PORTSC_PR, 0);
    assert_eq!(portsc & PORTSC_PED, 0);
    assert_eq!(portsc & PORTSC_LSDA, 0);

    for _ in 0..49 {
        hub.tick_1ms();
        let portsc = hub.read_portsc(0);
        assert_ne!(portsc & PORTSC_PR, 0);
        assert_eq!(portsc & PORTSC_PED, 0);
    }

    // After 50ms the reset auto-completes, PR clears, and the port is enabled again.
    hub.tick_1ms();
    let portsc = hub.read_portsc(0);
    assert_eq!(portsc & PORTSC_PR, 0);
    assert_ne!(portsc & PORTSC_PED, 0);

    // The device should have returned to address 0 after the bus reset.
    assert!(hub.device_mut_for_address(5).is_none());
    let dev = hub
        .device_mut_for_address(0)
        .expect("device should be visible at address 0 after reset");
    assert_eq!(dev.address(), 0);
}
