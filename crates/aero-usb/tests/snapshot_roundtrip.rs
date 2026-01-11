use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError};
use aero_usb::hid::passthrough::{UsbHidPassthrough, UsbHidPassthroughOutputReport};
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{
    SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
    UsbPassthroughDevice, UsbWebUsbPassthroughDevice,
};
use aero_usb::uhci::UhciController;
use aero_usb::usb::{SetupPacket, UsbDevice, UsbHandshake};

#[allow(dead_code)]
mod util;

use util::{TestIrq, TestMemory, LINK_PTR_T, PORTSC_PR, REG_FRBASEADD, REG_PORTSC1, REG_USBCMD};

const REG_FRNUM: u16 = 0x06;
const REG_SOFMOD: u16 = 0x0C;

const USBCMD_CF: u16 = 1 << 6;
const USBCMD_MAXP: u16 = 1 << 7;
const PORTSC_PED: u16 = 1 << 2;

fn control_no_data<D: UsbDevice>(dev: &mut D, setup: SetupPacket) {
    dev.handle_setup(setup);
    let mut zlp: [u8; 0] = [];
    assert!(
        matches!(dev.handle_in(0, &mut zlp), UsbHandshake::Ack { .. }),
        "expected ACK for status stage"
    );
}

fn control_in<D: UsbDevice>(dev: &mut D, setup: SetupPacket, expected_len: usize) -> Vec<u8> {
    dev.handle_setup(setup);
    let mut buf = vec![0u8; expected_len];
    let got = match dev.handle_in(0, &mut buf) {
        UsbHandshake::Ack { bytes } => bytes,
        other => panic!("expected ACK for control IN data stage, got {other:?}"),
    };
    buf.truncate(got);

    // Status stage for control-IN is an OUT ZLP.
    assert!(
        matches!(dev.handle_out(0, &[]), UsbHandshake::Ack { .. }),
        "expected ACK for control-IN status stage"
    );
    buf
}

fn control_out_data<D: UsbDevice>(dev: &mut D, setup: SetupPacket, data: &[u8]) {
    dev.handle_setup(setup);
    assert!(
        matches!(
            dev.handle_out(0, data),
            UsbHandshake::Ack { bytes } if bytes == data.len()
        ),
        "expected ACK for control OUT data stage"
    );

    // Status stage for control-OUT is an IN ZLP.
    let mut zlp: [u8; 0] = [];
    assert!(
        matches!(dev.handle_in(0, &mut zlp), UsbHandshake::Ack { bytes: 0 }),
        "expected ACK for control-OUT status stage"
    );
}

fn sample_report_descriptor_output_with_id() -> Vec<u8> {
    vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x00, // Usage (Undefined)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x02, // Report ID (2)
        0x09, 0x00, // Usage (Undefined)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x91, 0x02, // Output (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

#[test]
fn hid_passthrough_snapshot_roundtrip_preserves_state_and_input_queue() {
    let report_desc = sample_report_descriptor_output_with_id();
    let mut dev = UsbHidPassthrough::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc,
        true, // has interrupt OUT so we can exercise halt state without stalling IN.
        None,
        None,
        None,
    );

    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x05, // SET_ADDRESS
            value: 5,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x09, // SET_CONFIGURATION
            value: 1,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x03, // SET_FEATURE
            value: 1,      // DEVICE_REMOTE_WAKEUP
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x21,
            request: 0x0b, // SET_PROTOCOL
            value: 0,      // boot protocol
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x21,
            request: 0x0a, // SET_IDLE
            value: 7u16 << 8,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x02,
            request: 0x03, // SET_FEATURE
            value: 0,      // ENDPOINT_HALT
            index: 0x01,   // interrupt OUT endpoint address
            length: 0,
        },
    );

    dev.push_input_report(0, &[0x11, 0x22]);
    dev.push_input_report(0, &[0x33, 0x44]);

    // Queue an output report but do not drain it before snapshotting.
    control_out_data(
        &mut dev,
        SetupPacket {
            request_type: 0x21,
            request: 0x09,             // SET_REPORT
            value: (2u16 << 8) | 2u16, // Output report, ID 2
            index: 0,
            length: 3, // report ID + 2 bytes
        },
        &[2, 0xAA, 0xBB],
    );

    let snapshot = dev.save_state();

    let mut restored = UsbHidPassthrough::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_report_descriptor_output_with_id(),
        true,
        None,
        None,
        None,
    );
    restored
        .load_state(&snapshot)
        .expect("snapshot restore should succeed");

    assert_eq!(restored.address(), 5);
    assert!(restored.configured());

    // Remote wakeup should be restored (device GET_STATUS bit1).
    let status = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0x80,
            request: 0x00, // GET_STATUS
            value: 0,
            index: 0,
            length: 2,
        },
        2,
    );
    assert_eq!(status, [0x02, 0x00]);

    let protocol = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x03, // GET_PROTOCOL
            value: 0,
            index: 0,
            length: 1,
        },
        1,
    );
    assert_eq!(protocol, [0]);

    let idle = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x02, // GET_IDLE
            value: 0,
            index: 0,
            length: 1,
        },
        1,
    );
    assert_eq!(idle, [7]);

    // Interrupt OUT endpoint should remain halted.
    assert!(matches!(
        restored.handle_out(1, &[0x99]),
        UsbHandshake::Stall
    ));

    // Pending input reports should survive snapshot/restore and be served in order.
    let mut buf = [0u8; 8];
    assert!(matches!(
        restored.handle_in(1, &mut buf),
        UsbHandshake::Ack { bytes: 2 }
    ));
    assert_eq!(&buf[..2], [0x11, 0x22]);
    assert!(matches!(
        restored.handle_in(1, &mut buf),
        UsbHandshake::Ack { bytes: 2 }
    ));
    assert_eq!(&buf[..2], [0x33, 0x44]);
    assert!(matches!(restored.handle_in(1, &mut buf), UsbHandshake::Nak));

    // Pending output reports should survive snapshot/restore so host integrations can drain them.
    assert_eq!(
        restored.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 2,
            data: vec![0xAA, 0xBB],
        })
    );
    assert!(restored.pop_output_report().is_none());

    // The guest-visible "last output report" state should still be preserved for GET_REPORT.
    let out_report = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x01,             // GET_REPORT
            value: (2u16 << 8) | 2u16, // Output report, ID 2
            index: 0,
            length: 3,
        },
        3,
    );
    assert_eq!(out_report, [2, 0xAA, 0xBB]);
}

struct DummyUsbDevice;

impl UsbDevice for DummyUsbDevice {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn reset(&mut self) {}

    fn address(&self) -> u8 {
        0
    }

    fn handle_setup(&mut self, _setup: SetupPacket) {}

    fn handle_out(&mut self, _ep: u8, data: &[u8]) -> UsbHandshake {
        UsbHandshake::Ack { bytes: data.len() }
    }

    fn handle_in(&mut self, _ep: u8, _buf: &mut [u8]) -> UsbHandshake {
        UsbHandshake::Nak
    }
}

#[test]
fn hub_snapshot_roundtrip_preserves_port_reset_timer() {
    let mut hub = UsbHubDevice::new();

    control_no_data(
        &mut hub,
        SetupPacket {
            request_type: 0x00,
            request: 0x05, // SET_ADDRESS
            value: 3,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            request_type: 0x00,
            request: 0x09, // SET_CONFIGURATION
            value: 1,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            request_type: 0x00,
            request: 0x03, // SET_FEATURE
            value: 1,      // DEVICE_REMOTE_WAKEUP
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            request_type: 0x02,
            request: 0x03, // SET_FEATURE
            value: 0,      // ENDPOINT_HALT
            index: 0x81,   // interrupt IN endpoint address
            length: 0,
        },
    );

    // Attach something so the port reports a connection.
    hub.attach(1, Box::new(DummyUsbDevice));
    // Power and reset port 1.
    control_no_data(
        &mut hub,
        SetupPacket {
            request_type: 0x23,
            request: 0x03, // SET_FEATURE
            value: 8,      // PORT_POWER
            index: 1,
            length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            request_type: 0x23,
            request: 0x03, // SET_FEATURE
            value: 4,      // PORT_RESET
            index: 1,
            length: 0,
        },
    );

    for _ in 0..10 {
        hub.tick_1ms();
    }

    let port_status = control_in(
        &mut hub,
        SetupPacket {
            request_type: 0xA3,
            request: 0x00, // GET_STATUS
            value: 0,
            index: 1,
            length: 4,
        },
        4,
    );
    let st = u16::from_le_bytes([port_status[0], port_status[1]]);
    assert_ne!(st & (1 << 4), 0, "port reset should be active");

    let snapshot = hub.save_state();

    let mut restored = UsbHubDevice::new();
    restored
        .load_state(&snapshot)
        .expect("hub snapshot restore should succeed");

    assert_eq!(restored.address(), 3);
    let cfg = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0x80,
            request: 0x08, // GET_CONFIGURATION
            value: 0,
            index: 0,
            length: 1,
        },
        1,
    );
    assert_eq!(cfg, [1]);

    let status = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0x80,
            request: 0x00, // GET_STATUS
            value: 0,
            index: 0,
            length: 2,
        },
        2,
    );
    assert_eq!(status, [0x02, 0x00]);

    let mut intr_buf = [0u8; 8];
    assert_eq!(restored.handle_in(1, &mut intr_buf), UsbHandshake::Stall);

    // The reset countdown should survive restore: 40ms remaining.
    for _ in 0..39 {
        restored.tick_1ms();
    }
    let port_status = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA3,
            request: 0x00,
            value: 0,
            index: 1,
            length: 4,
        },
        4,
    );
    let st = u16::from_le_bytes([port_status[0], port_status[1]]);
    assert_ne!(st & (1 << 4), 0, "reset should still be active after 39ms");

    restored.tick_1ms();
    let port_status = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA3,
            request: 0x00,
            value: 0,
            index: 1,
            length: 4,
        },
        4,
    );
    let st = u16::from_le_bytes([port_status[0], port_status[1]]);
    assert_eq!(st & (1 << 4), 0, "reset should complete after 40ms");
    assert_ne!(st & (1 << 0), 0, "connection should remain present");
    assert_ne!(st & (1 << 1), 0, "port should be enabled after reset");
    assert_ne!(st & (1 << 8), 0, "port should remain powered");
}

#[test]
fn uhci_snapshot_roundtrip_preserves_regs_and_port_timer() {
    let io_base = 0x2000;
    let irq_line = 11;
    let mut ctrl = UhciController::new(io_base, irq_line);
    ctrl.connect_device(0, Box::new(DummyUsbDevice));

    let mut mem = TestMemory::new(0x4000);
    let mut irq = TestIrq::default();

    let fl_base = 0x1000;
    ctrl.port_write(io_base + REG_FRBASEADD, 4, fl_base, &mut irq);
    for i in 0..1024u32 {
        mem.write_u32(fl_base + i * 4, LINK_PTR_T);
    }

    // Start a port reset sequence (50ms).
    ctrl.port_write(io_base + REG_PORTSC1, 2, PORTSC_PR as u32, &mut irq);

    // Run the controller against an empty schedule so FRNUM advances deterministically.
    let usbcmd = util::USBCMD_RUN | USBCMD_CF | USBCMD_MAXP;
    ctrl.port_write(io_base + REG_USBCMD, 2, usbcmd as u32, &mut irq);

    ctrl.port_write(io_base + REG_FRNUM, 2, 0x0123, &mut irq);
    ctrl.port_write(io_base + REG_SOFMOD, 1, 0x55, &mut irq);

    for _ in 0..10 {
        ctrl.step_frame(&mut mem, &mut irq);
    }

    let expected_frnum = ctrl.port_read(io_base + REG_FRNUM, 2);
    let expected_portsc1 = ctrl.port_read(io_base + REG_PORTSC1, 2) as u16;
    assert_ne!(
        expected_portsc1 & PORTSC_PR,
        0,
        "reset should still be active"
    );

    let snapshot = ctrl.save_state();

    let mut restored = UhciController::new(0x3000, 5);
    restored
        .load_state(&snapshot)
        .expect("uhci snapshot restore should succeed");

    assert_eq!(restored.io_base(), io_base);
    assert_eq!(restored.irq_line(), irq_line);
    assert_eq!(restored.port_read(io_base + REG_FRBASEADD, 4), fl_base);
    assert_eq!(restored.port_read(io_base + REG_FRNUM, 2), expected_frnum);
    assert_eq!(restored.port_read(io_base + REG_SOFMOD, 1), 0x55);
    assert_eq!(
        restored.port_read(io_base + REG_PORTSC1, 2) as u16,
        expected_portsc1
    );

    let root0 = restored.bus().port(0).unwrap();
    assert!(root0.connected, "root port connection must be preserved");
    assert!(
        !root0.enabled,
        "root port should remain disabled during reset"
    );

    // Continue the reset timer: 40ms remaining.
    for _ in 0..39 {
        restored.step_frame(&mut mem, &mut irq);
    }
    let portsc1 = restored.port_read(io_base + REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc1 & PORTSC_PR,
        0,
        "reset should still be active after 39ms"
    );

    restored.step_frame(&mut mem, &mut irq);
    let portsc1 = restored.port_read(io_base + REG_PORTSC1, 2) as u16;
    assert_eq!(portsc1 & PORTSC_PR, 0, "reset bit clears after 40ms");
    assert_ne!(
        portsc1 & PORTSC_PED,
        0,
        "port should be enabled after reset completes"
    );
    assert!(restored.bus().port(0).unwrap().enabled);
}

#[test]
fn snapshot_device_id_mismatch_returns_error() {
    let dev = UsbHidPassthrough::default();
    let snapshot = dev.save_state();
    let mut corrupted = snapshot.clone();
    corrupted[8..12].copy_from_slice(b"NOPE");

    let mut restored = UsbHidPassthrough::default();
    let err = restored.load_state(&corrupted).unwrap_err();
    assert!(matches!(err, SnapshotError::DeviceIdMismatch { .. }));
}

#[test]
fn snapshot_major_version_mismatch_returns_error() {
    let dev = UsbHidPassthrough::default();
    let snapshot = dev.save_state();
    let mut corrupted = snapshot.clone();
    corrupted[12..14].copy_from_slice(&2u16.to_le_bytes());

    let mut restored = UsbHidPassthrough::default();
    let err = restored.load_state(&corrupted).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::UnsupportedDeviceMajorVersion {
            found: 2,
            supported: 1
        }
    ));
}

#[test]
fn snapshot_minor_version_mismatch_is_accepted() {
    let dev = UsbHidPassthrough::default();
    let snapshot = dev.save_state();
    let mut corrupted = snapshot.clone();
    corrupted[14..16].copy_from_slice(&42u16.to_le_bytes());

    let mut restored = UsbHidPassthrough::default();
    restored
        .load_state(&corrupted)
        .expect("minor version mismatch should be accepted");
}

#[test]
fn snapshot_unknown_fields_are_ignored() {
    let mut dev = UsbHidPassthrough::default();

    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x09, // SET_CONFIGURATION
            value: 1,
            index: 0,
            length: 0,
        },
    );
    dev.push_input_report(0, &[0x11, 0x22]);
    dev.push_input_report(0, &[0x33, 0x44]);

    let snapshot = dev.save_state();
    let mut extended = snapshot.clone();

    let tag = 999u16;
    let payload = [1u8, 2, 3, 4];
    extended.extend_from_slice(&tag.to_le_bytes());
    extended.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    extended.extend_from_slice(&payload);

    let mut restored = UsbHidPassthrough::default();
    restored
        .load_state(&extended)
        .expect("unknown TLV tags should be ignored");
    assert!(restored.configured());

    let mut buf = [0u8; 8];
    assert!(matches!(
        restored.handle_in(1, &mut buf),
        UsbHandshake::Ack { bytes: 2 }
    ));
    assert_eq!(&buf[..2], [0x11, 0x22]);
    assert!(matches!(
        restored.handle_in(1, &mut buf),
        UsbHandshake::Ack { bytes: 2 }
    ));
    assert_eq!(&buf[..2], [0x33, 0x44]);
}

#[test]
fn usb_passthrough_device_snapshot_preserves_next_id_and_drops_pending_io() {
    let mut dev = UsbPassthroughDevice::new();

    // Queue a bulk IN action (id=1) and leave it in-flight.
    dev.handle_in_transfer(0x81, 8);
    assert_eq!(dev.pending_summary().queued_actions, 1);
    let id1 = match dev.pop_action().expect("expected queued action") {
        UsbHostAction::BulkIn {
            id,
            endpoint,
            length,
        } => {
            assert_eq!(endpoint, 0x81);
            assert_eq!(length, 8);
            id
        }
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(id1, 1);
    assert_eq!(dev.pending_summary().inflight_endpoints, 1);

    let snapshot = dev.save_state();

    let mut restored = UsbPassthroughDevice::new();
    restored
        .load_state(&snapshot)
        .expect("passthrough snapshot restore should succeed");

    let summary = restored.pending_summary();
    assert_eq!(summary.queued_actions, 0);
    assert_eq!(summary.queued_completions, 0);
    assert_eq!(summary.inflight_control, None);
    assert_eq!(summary.inflight_endpoints, 0);

    // Next action should continue from next_id=2.
    restored.handle_in_transfer(0x81, 8);
    let id2 = match restored
        .pop_action()
        .expect("expected action after restore")
    {
        UsbHostAction::BulkIn { id, .. } => id,
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(id2, 2);
}

#[test]
fn webusb_passthrough_device_snapshot_preserves_pending_set_address() {
    let mut dev = UsbWebUsbPassthroughDevice::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x05, // SET_ADDRESS
        value: 12,
        index: 0,
        length: 0,
    });
    assert_eq!(dev.address(), 0);

    let snapshot = dev.save_state();

    let mut restored = UsbWebUsbPassthroughDevice::new();
    restored
        .load_state(&snapshot)
        .expect("webusb snapshot restore should succeed");

    assert_eq!(restored.address(), 0);

    // Status stage for SET_ADDRESS is an IN ZLP.
    let mut zlp: [u8; 0] = [];
    assert_eq!(
        restored.handle_in(0, &mut zlp),
        UsbHandshake::Ack { bytes: 0 }
    );
    assert_eq!(restored.address(), 12);
}

#[test]
fn webusb_passthrough_device_snapshot_requeues_control_in_action() {
    let mut dev = UsbWebUsbPassthroughDevice::new();

    let setup = SetupPacket {
        request_type: 0x80,
        request: 0x06, // GET_DESCRIPTOR
        value: 0x0100,
        index: 0,
        length: 4,
    };
    dev.handle_setup(setup);

    let actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let id1 = match actions[0] {
        UsbHostAction::ControlIn {
            id,
            setup: host_setup,
        } => {
            assert_eq!(
                host_setup,
                HostSetupPacket {
                    bm_request_type: setup.request_type,
                    b_request: setup.request,
                    w_value: setup.value,
                    w_index: setup.index,
                    w_length: setup.length,
                }
            );
            id
        }
        ref other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(id1, 1);

    let snapshot = dev.save_state();

    let mut restored = UsbWebUsbPassthroughDevice::new();
    restored
        .load_state(&snapshot)
        .expect("webusb snapshot restore should succeed");
    restored.reset_host_state_for_restore();

    // First poll should NAK and re-queue a fresh host action.
    let mut buf = [0u8; 4];
    assert_eq!(restored.handle_in(0, &mut buf), UsbHandshake::Nak);

    let actions = restored.drain_actions();
    assert_eq!(actions.len(), 1);
    let id2 = match actions[0] {
        UsbHostAction::ControlIn {
            id,
            setup: host_setup,
        } => {
            assert_eq!(
                host_setup,
                HostSetupPacket {
                    bm_request_type: setup.request_type,
                    b_request: setup.request,
                    w_value: setup.value,
                    w_index: setup.index,
                    w_length: setup.length,
                }
            );
            id
        }
        ref other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(id2, 2);

    restored.push_completion(UsbHostCompletion::ControlIn {
        id: id2,
        result: UsbHostCompletionIn::Success {
            data: vec![9, 8, 7, 6],
        },
    });

    assert_eq!(
        restored.handle_in(0, &mut buf),
        UsbHandshake::Ack { bytes: 4 }
    );
    assert_eq!(buf, [9, 8, 7, 6]);

    // Status stage for control-IN is an OUT ZLP.
    assert_eq!(restored.handle_out(0, &[]), UsbHandshake::Ack { bytes: 0 });
}
