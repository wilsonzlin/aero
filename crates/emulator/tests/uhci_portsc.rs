use emulator::io::usb::core::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use emulator::io::usb::hub::{RootHub, UsbHubDevice};
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
}

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(d) if d.is_empty()),
        "expected status stage to return empty IN packet",
    );
}

fn control_in(dev: &mut AttachedUsbDevice, setup: SetupPacket) -> Vec<u8> {
    let max_len = setup.w_length as usize;
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    let data = match dev.handle_in(0, max_len) {
        UsbInResult::Data(data) => data,
        other => panic!("expected control IN data stage, got {other:?}"),
    };
    // Complete the status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    data
}

fn set_address(dev: &mut AttachedUsbDevice, address: u8) {
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x05, // SET_ADDRESS
        w_value: address as u16,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(matches!(dev.handle_in(0, 0), UsbInResult::Data(d) if d.is_empty()));
    assert_eq!(dev.address(), address);
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

#[test]
fn uhci_portsc_write_enable_ignored_when_disconnected() {
    const PORTSC_CCS: u16 = 1 << 0;
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_PEDC: u16 = 1 << 3;

    let mut hub = RootHub::new();

    // With no device present, attempting to enable the port should have no effect.
    hub.write_portsc(0, PORTSC_PED);
    let st = hub.read_portsc(0);
    assert_eq!(st & PORTSC_CCS, 0);
    assert_eq!(st & PORTSC_PED, 0);
    assert_eq!(st & PORTSC_PEDC, 0);
}

#[test]
fn uhci_portsc_port_reset_propagates_to_external_hub_and_downstream_devices() {
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_PR: u16 = 1 << 9;

    // Hub feature selectors.
    const HUB_PORT_FEATURE_ENABLE: u16 = 1;
    const HUB_PORT_FEATURE_RESET: u16 = 4;
    const HUB_PORT_FEATURE_POWER: u16 = 8;

    // Hub port status bits.
    const HUB_PORT_STATUS_CONNECTION: u16 = 1 << 0;
    const HUB_PORT_STATUS_ENABLE: u16 = 1 << 1;

    let mut root = RootHub::new();

    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(TestUsbDevice));
    root.attach(0, Box::new(hub));
    root.force_enable_for_tests(0);

    // Enumerate the hub at address 1 and configure it.
    {
        let hub_dev = root
            .device_mut_for_address(0)
            .expect("hub should be reachable at address 0");
        set_address(hub_dev, 1);
    }
    {
        let hub_dev = root
            .device_mut_for_address(1)
            .expect("hub should be reachable at address 1");
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );

        // Power + reset downstream port 1 to make the attached device reachable.
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x23, // HostToDevice | Class | Other
                b_request: 0x03,       // SET_FEATURE
                w_value: HUB_PORT_FEATURE_POWER,
                w_index: 1, // port 1
                w_length: 0,
            },
        );
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x23, // HostToDevice | Class | Other
                b_request: 0x03,       // SET_FEATURE
                w_value: HUB_PORT_FEATURE_RESET,
                w_index: 1, // port 1
                w_length: 0,
            },
        );
    }
    // Wait for the hub's downstream port reset timer.
    for _ in 0..50 {
        root.tick_1ms();
    }

    // Assign address 5 to the downstream device (reachable at default address 0).
    {
        let downstream = root
            .device_mut_for_address(0)
            .expect("downstream device should be reachable at address 0");
        set_address(downstream, 5);
    }
    assert!(root.device_mut_for_address(5).is_some());

    // Trigger a bus reset on the root port. This should reset the hub, which must in turn reset
    // downstream device address state.
    root.write_portsc(0, PORTSC_PR | PORTSC_PED);
    for _ in 0..50 {
        root.tick_1ms();
    }

    // Hub should have returned to the default-address state and be unconfigured.
    assert!(root.device_mut_for_address(1).is_none());
    {
        let hub_dev = root
            .device_mut_for_address(0)
            .expect("hub should be reachable at address 0 after reset");
        let cfg = control_in(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x80, // DeviceToHost | Standard | Device
                b_request: 0x08,       // GET_CONFIGURATION
                w_value: 0,
                w_index: 0,
                w_length: 1,
            },
        );
        assert_eq!(cfg, vec![0]);

        // Hub port should be disabled after the upstream reset, while still showing the device as
        // connected.
        let status = control_in(
            hub_dev,
            SetupPacket {
                bm_request_type: 0xa3, // DeviceToHost | Class | Other
                b_request: 0x00,       // GET_STATUS
                w_value: 0,
                w_index: 1,
                w_length: 4,
            },
        );
        let port_status = u16::from_le_bytes([status[0], status[1]]);
        assert_ne!(port_status & HUB_PORT_STATUS_CONNECTION, 0);
        assert_eq!(port_status & HUB_PORT_STATUS_ENABLE, 0);

        // Even though the port is now disabled and not routable, the attached device should have
        // been reset back to address 0.
        let downstream = hub_dev
            .as_hub_mut()
            .unwrap()
            .downstream_device_mut(0)
            .expect("downstream device still attached");
        assert_eq!(downstream.address(), 0);
    }
    assert!(root.device_mut_for_address(5).is_none());

    // Re-enumerate hub and re-enable the downstream port *without* another downstream port reset;
    // the previously assigned downstream address must not become routable again.
    {
        let hub_dev = root.device_mut_for_address(0).unwrap();
        set_address(hub_dev, 1);
    }
    {
        let hub_dev = root.device_mut_for_address(1).unwrap();
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x23,
                b_request: 0x03,
                w_value: HUB_PORT_FEATURE_POWER,
                w_index: 1,
                w_length: 0,
            },
        );
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x23,
                b_request: 0x03,
                w_value: HUB_PORT_FEATURE_ENABLE,
                w_index: 1,
                w_length: 0,
            },
        );
    }
    assert!(root.device_mut_for_address(0).is_some());
    assert!(root.device_mut_for_address(5).is_none());
}
