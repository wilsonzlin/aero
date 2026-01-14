use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::xhci::{XhciController, PORTSC_PR};
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult, UsbWebUsbPassthroughDevice};

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_in(dev: &mut AttachedUsbDevice, setup: SetupPacket) -> Vec<u8> {
    let expected_len = setup.w_length as usize;
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let mut out = Vec::new();
    loop {
        match dev.handle_in(0, 64) {
            UsbInResult::Data(chunk) => {
                out.extend_from_slice(&chunk);
                if chunk.len() < 64 || out.len() >= expected_len {
                    break;
                }
            }
            UsbInResult::Nak => continue,
            UsbInResult::Stall => panic!("unexpected STALL during control IN transfer"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control IN transfer"),
        }
    }

    // Status stage for control-IN is an OUT ZLP.
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    out.truncate(expected_len);
    out
}

#[test]
fn xhci_snapshot_roundtrip_preserves_reset_timer_and_usb_topology() {
    let mut ctrl = XhciController::new();

    // Root port 0: external hub with a keyboard on hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    let mut hub = UsbHubDevice::new_with_ports(4);
    hub.attach(1, Box::new(keyboard.clone()));
    ctrl.attach_device(0, Box::new(hub));

    // Root port 1: attach another device so root port resets can start.
    ctrl.attach_device(1, Box::new(UsbHidKeyboardHandle::new()));

    // Root port 2: WebUSB passthrough device used to keep a control transfer pending (NAK).
    ctrl.attach_device(2, Box::new(UsbWebUsbPassthroughDevice::new()));

    // Reset + enable root port 0 so the hub's internal timers tick.
    ctrl.write_portsc(0, PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms_no_dma();
    }

    // Enumerate hub (addr=1, cfg=1).
    {
        let hub_dev = ctrl
            .port_device_mut(0)
            .expect("expected hub on root port 0");
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x05, // SET_ADDRESS
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
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

        // Hub port 1: power + reset.
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x23,
                b_request: 0x03, // SET_FEATURE
                w_value: 8,      // PORT_POWER
                w_index: 1,
                w_length: 0,
            },
        );
        control_no_data(
            hub_dev,
            SetupPacket {
                bm_request_type: 0x23,
                b_request: 0x03, // SET_FEATURE
                w_value: 4,      // PORT_RESET
                w_index: 1,
                w_length: 0,
            },
        );
    }
    for _ in 0..50 {
        ctrl.tick_1ms_no_dma();
    }

    // Enumerate downstream keyboard to addr=2, cfg=1.
    {
        let hub_dev = ctrl
            .port_device_mut(0)
            .expect("expected hub on root port 0");
        let kb_dev = hub_dev
            .model_mut()
            .hub_port_device_mut(1)
            .expect("expected keyboard on hub port 1");
        control_no_data(
            kb_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x05, // SET_ADDRESS
                w_value: 2,
                w_index: 0,
                w_length: 0,
            },
        );
        control_no_data(
            kb_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
    }

    // Start a root-hub port reset on port 1 and advance it partially.
    ctrl.write_portsc(1, PORTSC_PR);
    for _ in 0..10 {
        ctrl.tick_1ms_no_dma();
    }
    assert_ne!(ctrl.read_portsc(1) & PORTSC_PR, 0);

    // Start a control-IN transfer to the WebUSB passthrough device. With no host completion, the
    // device returns NAK and keeps the control transfer pending, exercising snapshot/restore of
    // in-flight control state and nested device model snapshots.
    {
        let dev = ctrl
            .port_device_mut(2)
            .expect("expected WebUSB device on root port 2");
        assert_eq!(
            dev.handle_setup(SetupPacket {
                bm_request_type: 0x80, // DeviceToHost | Standard | Device
                b_request: 0x06,       // GET_DESCRIPTOR
                w_value: 0x0100,       // DEVICE descriptor
                w_index: 0,
                w_length: 18,
            }),
            UsbOutResult::Ack
        );
        assert!(
            matches!(dev.handle_in(0, 64), UsbInResult::Nak),
            "expected NAK while passthrough control transfer is in-flight"
        );
    }

    let pending_before = ctrl.pending_event_count();
    let dropped_before = ctrl.dropped_event_trbs();
    let irq_before = ctrl.irq_level();

    let snap1 = ctrl.save_state();
    let snap2 = ctrl.save_state();
    assert_eq!(snap2, snap1, "xhci snapshot should be deterministic");

    // Restore into a fresh controller without pre-attaching devices.
    let mut restored = XhciController::new();
    restored.load_state(&snap1).unwrap();

    // Snapshot bytes must be deterministic after restore as well.
    let restored_snap1 = restored.save_state();
    let restored_snap2 = restored.save_state();
    assert_eq!(
        restored_snap2, restored_snap1,
        "restored xhci snapshot should be deterministic"
    );
    assert_eq!(restored.pending_event_count(), pending_before);
    assert_eq!(restored.dropped_event_trbs(), dropped_before);
    assert_eq!(restored.irq_level(), irq_before);

    // Pending NAK control transfer should remain pending after restore.
    {
        let dev = restored
            .port_device_mut(2)
            .expect("expected WebUSB device on restored root port 2");
        assert!(
            matches!(dev.handle_in(0, 64), UsbInResult::Nak),
            "expected NAK on restored pending control transfer"
        );
    }

    // Device address/configured state should be preserved (GET_CONFIGURATION => 1).
    {
        let hub_dev = restored
            .port_device_mut(0)
            .expect("expected hub on restored root port 0");
        assert_eq!(hub_dev.address(), 1);
        let kb_dev = hub_dev
            .model_mut()
            .hub_port_device_mut(1)
            .expect("expected keyboard on restored hub port 1");
        assert_eq!(kb_dev.address(), 2);

        let cfg = control_in(
            kb_dev,
            SetupPacket {
                bm_request_type: 0x80, // DeviceToHost | Standard | Device
                b_request: 0x08,       // GET_CONFIGURATION
                w_value: 0,
                w_index: 0,
                w_length: 1,
            },
        );
        assert_eq!(cfg, vec![1]);
    }

    // Port reset timer remaining time should be preserved.
    let mut remaining = 0usize;
    while restored.read_portsc(1) & PORTSC_PR != 0 {
        restored.tick_1ms_no_dma();
        remaining += 1;
        assert!(
            remaining <= 50,
            "root port reset should complete within 50ms"
        );
    }
    assert_eq!(
        remaining, 40,
        "expected 40ms remaining on restored port reset"
    );
}
