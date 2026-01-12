use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotVersion, SnapshotWriter};
use aero_usb::hid::{UsbHidKeyboardHandle, UsbHidMouseHandle};
use aero_usb::hub::UsbHubDevice;
use aero_usb::uhci::UhciController;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};
use core::any::Any;

mod util;

use util::{TestMemory, PORTSC_PR, REG_PORTSC1};

fn control_no_data(ctrl: &mut UhciController, addr: u8, setup: SetupPacket) {
    let dev = ctrl
        .hub_mut()
        .device_mut_for_address(addr)
        .unwrap_or_else(|| panic!("expected USB device at address {addr}"));
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn external_hub_ref(ctrl: &UhciController) -> &UsbHubDevice {
    let dev = ctrl
        .hub()
        .port_device(0)
        .expect("expected device on root port 0");
    let any = dev.model() as &dyn Any;
    any.downcast_ref::<UsbHubDevice>()
        .expect("expected UsbHubDevice on root port 0")
}

fn external_hub_mut(ctrl: &mut UhciController) -> &mut UsbHubDevice {
    let dev = ctrl
        .hub_mut()
        .port_device_mut(0)
        .expect("expected device on root port 0");
    let any = dev.model_mut() as &mut dyn Any;
    any.downcast_mut::<UsbHubDevice>()
        .expect("expected UsbHubDevice on root port 0")
}

#[test]
fn snapshot_roundtrip_is_deterministic_and_preserves_external_hub_reports() {
    let mut ctrl = UhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(UsbHubDevice::new_with_ports(4)));

    let kb = UsbHidKeyboardHandle::new();
    let mouse = UsbHidMouseHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(kb.clone()))
        .expect("attach keyboard");
    ctrl.hub_mut()
        .attach_at_path(&[0, 2], Box::new(mouse.clone()))
        .expect("attach mouse");

    let mut mem = TestMemory::new(0x20_000);

    // Reset + enable root port 0 so the bus routes packets.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Enumerate hub (addr=1, cfg=1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Hub port 1: power + reset, enumerate downstream keyboard to addr=2.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: 8,      // PORT_POWER
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: 4,      // PORT_RESET
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Hub port 2: power + reset, enumerate downstream mouse to addr=3.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03,
            w_value: 8,
            w_index: 2,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03,
            w_value: 4,
            w_index: 2,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 3,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Queue one report in each downstream device.
    kb.key_event(0x04, true); // 'a'
    mouse.movement(1, 1);

    let ctrl_snap1 = ctrl.save_state();
    let hub_snap1 = external_hub_ref(&ctrl).save_state();
    let kb_snap1 = kb.save_state();
    let mouse_snap1 = mouse.save_state();

    // Restore the controller + device tree from snapshots.
    let mut restored_ctrl = UhciController::new();
    restored_ctrl
        .hub_mut()
        .attach(0, Box::new(UsbHubDevice::new_with_ports(4)));

    let mut restored_kb = UsbHidKeyboardHandle::new();
    restored_kb.load_state(&kb_snap1).unwrap();
    let mut restored_mouse = UsbHidMouseHandle::new();
    restored_mouse.load_state(&mouse_snap1).unwrap();

    restored_ctrl
        .hub_mut()
        .attach_at_path(&[0, 1], Box::new(restored_kb.clone()))
        .unwrap();
    restored_ctrl
        .hub_mut()
        .attach_at_path(&[0, 2], Box::new(restored_mouse.clone()))
        .unwrap();

    external_hub_mut(&mut restored_ctrl)
        .load_state(&hub_snap1)
        .unwrap();
    restored_ctrl.load_state(&ctrl_snap1).unwrap();

    assert_eq!(
        restored_ctrl.save_state(),
        ctrl_snap1,
        "UHCI controller snapshot should roundtrip byte-for-byte"
    );
    assert_eq!(
        external_hub_ref(&restored_ctrl).save_state(),
        hub_snap1,
        "external hub snapshot should roundtrip byte-for-byte"
    );
    assert_eq!(
        restored_kb.save_state(),
        kb_snap1,
        "keyboard model snapshot should roundtrip byte-for-byte"
    );
    assert_eq!(
        restored_mouse.save_state(),
        mouse_snap1,
        "mouse model snapshot should roundtrip byte-for-byte"
    );

    let kb_dev = restored_ctrl
        .hub_mut()
        .device_mut_for_address(2)
        .expect("expected keyboard at address 2");
    let kb_report = match kb_dev.handle_in(1, 8) {
        UsbInResult::Data(data) => data,
        other => panic!("expected keyboard interrupt IN report, got {other:?}"),
    };
    assert_eq!(kb_report.len(), 8);
    assert_eq!(kb_report[2], 0x04);

    let mouse_dev = restored_ctrl
        .hub_mut()
        .device_mut_for_address(3)
        .expect("expected mouse at address 3");
    let mouse_report = match mouse_dev.handle_in(1, 4) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse interrupt IN report, got {other:?}"),
    };
    assert!(mouse_report.len() >= 3);
    assert_eq!(mouse_report[1], 1);
    assert_eq!(mouse_report[2], 1);
}

#[test]
fn snapshot_restore_rejects_truncated_bytes() {
    let ctrl = UhciController::new();
    let snap = ctrl.save_state();

    for len in [0usize, 1, snap.len().saturating_sub(1)] {
        let mut restored = UhciController::new();
        let err = restored.load_state(&snap[..len]).unwrap_err();
        assert!(matches!(err, SnapshotError::UnexpectedEof));
    }
}

#[test]
fn snapshot_restore_rejects_oversized_usb_device_snapshots() {
    // TAG_ROOT_HUB_PORTS from `uhci::UhciController` snapshot encoding.
    const TAG_ROOT_HUB_PORTS: u16 = 8;
    const MAX_USB_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

    // Construct a root hub ports snapshot that declares a device snapshot larger than the
    // defensive cap. The bytes themselves are omitted; the loader should reject the declared
    // length first.
    let port0 = Encoder::new()
        .bool(false) // connected
        .bool(false) // connect_change
        .bool(false) // enabled
        .bool(false) // enable_change
        .bool(false) // resume_detect
        .bool(false) // reset
        .u8(0) // reset_countdown_ms
        .bool(false) // suspended
        .bool(false) // resuming
        .u8(0) // resume_countdown_ms
        .bool(true) // has device snapshot
        .u32((MAX_USB_SNAPSHOT_BYTES + 1) as u32)
        .finish();
    let port1 = Encoder::new()
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .u8(0)
        .bool(false)
        .bool(false)
        .u8(0)
        .bool(false)
        .finish();

    let ports = Encoder::new().vec_bytes(&[port0, port1]).finish();

    let mut w = SnapshotWriter::new(*b"UHCI", SnapshotVersion::new(1, 0));
    w.field_bytes(TAG_ROOT_HUB_PORTS, ports);
    let snapshot = w.finish();

    let mut restored = UhciController::new();
    let err = restored.load_state(&snapshot).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::InvalidFieldEncoding("usb device snapshot too large")
    ));
}
