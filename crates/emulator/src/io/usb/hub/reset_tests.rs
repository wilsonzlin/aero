use crate::io::usb::core::{UsbInResult, UsbOutResult};
use crate::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel};

use super::{UsbHub, UsbHubDevice};

#[derive(Debug, Default)]
struct DummyDevice;

impl UsbDeviceModel for DummyDevice {
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

fn setup(
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    w_length: u16,
) -> SetupPacket {
    SetupPacket {
        bm_request_type,
        b_request,
        w_value,
        w_index,
        w_length,
    }
}

fn assert_control_data(resp: ControlResponse, expected: &[u8]) {
    match resp {
        ControlResponse::Data(data) => assert_eq!(data, expected),
        other => panic!("expected ControlResponse::Data, got {other:?}"),
    }
}

fn port_status_and_change(hub: &mut UsbHubDevice, port: u16) -> (u16, u16) {
    let resp = hub.handle_control_request(setup(0xA3, 0x00, 0, port, 4), None);
    let data = match resp {
        ControlResponse::Data(data) => data,
        other => panic!("expected port GET_STATUS data, got {other:?}"),
    };
    let status = u16::from_le_bytes([data[0], data[1]]);
    let change = u16::from_le_bytes([data[2], data[3]]);
    (status, change)
}

fn port_enabled(hub: &mut UsbHubDevice, port: u16) -> bool {
    let (status, _) = port_status_and_change(hub, port);
    status & (1 << 1) != 0
}

#[test]
fn hub_reset_clears_configuration_disables_ports_and_resets_downstream_addresses() {
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(DummyDevice::default()));

    // Configure the hub (SET_CONFIGURATION(1)).
    assert_eq!(
        hub.handle_control_request(setup(0x00, 0x09, 1, 0, 0), None),
        ControlResponse::Ack
    );

    // Power the port and run a port reset so the downstream device becomes reachable.
    assert_eq!(
        hub.handle_control_request(setup(0x23, 0x03, 8, 1, 0), None),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(setup(0x23, 0x03, 4, 1, 0), None),
        ControlResponse::Ack
    );
    for _ in 0..50 {
        UsbHub::tick_1ms(&mut hub);
    }
    assert!(port_enabled(&mut hub, 1));

    // Clear change bits so the upstream reset can be observed through the hub interrupt bitmap.
    assert_eq!(
        hub.handle_control_request(setup(0x23, 0x01, 16, 1, 0), None),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(setup(0x23, 0x01, 17, 1, 0), None),
        ControlResponse::Ack
    );

    // Assign a non-zero address to the downstream device.
    {
        let dev = hub
            .downstream_device_mut_for_address(0)
            .expect("downstream device at address 0");
        assert_eq!(
            dev.handle_setup(setup(0x00, 0x05, 5, 0, 0)),
            UsbOutResult::Ack
        );
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
    }
    assert!(hub.downstream_device_mut_for_address(5).is_some());

    // Simulate an upstream bus reset.
    hub.reset();

    // Hub must return to default, unconfigured state.
    assert_control_data(
        hub.handle_control_request(setup(0x80, 0x08, 0, 0, 1), None),
        &[0],
    );

    // Enabled ports should be disabled by the upstream reset.
    assert!(!port_enabled(&mut hub, 1));

    let (_, change) = port_status_and_change(&mut hub, 1);
    assert_ne!(
        change & (1 << 0),
        0,
        "connect_change should be set after reset"
    );
    assert_ne!(
        change & (1 << 1),
        0,
        "enable_change should be set after reset"
    );

    // Downstream device state must be reset (address returns to 0) even though the port is now
    // disabled and not routable.
    assert_eq!(
        hub.downstream_device_mut(0)
            .expect("device still attached")
            .address(),
        0
    );
    assert!(hub.downstream_device_mut_for_address(5).is_none());

    // Reconfigure hub + re-enable the port without issuing another downstream port reset. If the
    // hub fails to reset downstream device addresses, the old address would become routable again
    // here.
    assert_eq!(
        hub.handle_control_request(setup(0x00, 0x09, 1, 0, 0), None),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(setup(0x23, 0x03, 8, 1, 0), None),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(setup(0x23, 0x03, 1, 1, 0), None),
        ControlResponse::Ack
    );

    assert!(port_enabled(&mut hub, 1));
    assert!(hub.downstream_device_mut_for_address(0).is_some());
    assert!(hub.downstream_device_mut_for_address(5).is_none());
}

#[test]
fn hub_interrupt_endpoint_naks_when_unconfigured() {
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(DummyDevice::default()));

    // Configure: interrupt endpoint should report the initial connection change bitmap.
    assert_eq!(
        hub.handle_control_request(setup(0x00, 0x09, 1, 0, 0), None),
        ControlResponse::Ack
    );
    assert!(hub.poll_interrupt_in(0x81).is_some());

    // Upstream reset must force interrupt IN to NAK when the hub is unconfigured.
    hub.reset();
    assert!(hub.poll_interrupt_in(0x81).is_none());
}
