use std::cell::RefCell;
use std::rc::Rc;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::{UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{
    SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
    UsbPassthroughDevice, UsbWebUsbPassthroughDevice,
};
use aero_usb::uhci::regs::{REG_FRNUM, REG_SOFMOD, USBCMD_CF, USBCMD_MAXP};
use aero_usb::uhci::UhciController;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};

mod util;

use util::{TestMemory, LINK_PTR_T, PORTSC_PR, REG_FRBASEADD, REG_PORTSC1, REG_USBCMD, USBCMD_RUN};

const PORTSC_PED: u16 = 1 << 2;

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_in(dev: &mut AttachedUsbDevice, setup: SetupPacket, expected_len: usize) -> Vec<u8> {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let mut out = Vec::new();
    loop {
        match dev.handle_in(0, 64) {
            UsbInResult::Data(chunk) => {
                out.extend_from_slice(&chunk);
                if chunk.len() < 64 {
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

fn control_out_data(dev: &mut AttachedUsbDevice, setup: SetupPacket, data: &[u8]) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert_eq!(dev.handle_out(0, data), UsbOutResult::Ack);

    // Status stage for control-OUT is an IN ZLP. Asynchronous models may NAK the status stage
    // until they have completed host-side work, so poll until we get the ZLP.
    loop {
        match dev.handle_in(0, 0) {
            UsbInResult::Data(resp) => {
                assert!(resp.is_empty(), "expected ZLP for status stage");
                break;
            }
            UsbInResult::Nak => continue,
            UsbInResult::Stall => panic!("unexpected STALL during control OUT status stage"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control OUT status stage"),
        }
    }
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

fn sample_report_descriptor_input_2_bytes() -> Vec<u8> {
    vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (0x01)
        0xa1, 0x01, // Collection (Application)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

#[derive(Default)]
struct DummyUsbDevice;

impl UsbDeviceModel for DummyUsbDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_out_transfer(&mut self, _ep: u8, _data: &[u8]) -> UsbOutResult {
        UsbOutResult::Ack
    }

    fn handle_in_transfer(&mut self, _ep: u8, _max_len: usize) -> UsbInResult {
        UsbInResult::Nak
    }
}

#[test]
fn hid_passthrough_snapshot_roundtrip_preserves_state_and_input_queue() {
    let report_desc = sample_report_descriptor_output_with_id();
    let dev_handle = UsbHidPassthroughHandle::new(
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
    let mut dev = AttachedUsbDevice::new(Box::new(dev_handle.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 5,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 1,      // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x0b, // SET_PROTOCOL
            w_value: 0,      // boot protocol
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x0a, // SET_IDLE
            w_value: 7u16 << 8,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x02,
            b_request: 0x03, // SET_FEATURE
            w_value: 0,      // ENDPOINT_HALT
            w_index: 0x01,   // interrupt OUT endpoint address
            w_length: 0,
        },
    );

    dev_handle.push_input_report(0, &[0x11, 0x22]);
    dev_handle.push_input_report(0, &[0x33, 0x44]);

    // Queue an output report but do not drain it before snapshotting.
    control_out_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x09,             // SET_REPORT
            w_value: (2u16 << 8) | 2u16, // Output report, ID 2
            w_index: 0,
            w_length: 3, // report ID + 2 bytes
        },
        &[2, 0xAA, 0xBB],
    );

    let model_snapshot = dev_handle.save_state();
    let dev_snapshot = dev.save_state();

    let mut restored_handle = UsbHidPassthroughHandle::new(
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
    restored_handle
        .load_state(&model_snapshot)
        .expect("snapshot restore should succeed");

    let mut restored = AttachedUsbDevice::new(Box::new(restored_handle.clone()));
    restored
        .load_state(&dev_snapshot)
        .expect("snapshot restore should succeed");

    assert_eq!(restored.address(), 5);
    assert!(restored_handle.configured());

    // Remote wakeup should be restored (device GET_STATUS bit1).
    let status = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x00, // GET_STATUS
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
        2,
    );
    assert_eq!(status, [0x02, 0x00]);

    let protocol = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x03, // GET_PROTOCOL
            w_value: 0,
            w_index: 0,
            w_length: 1,
        },
        1,
    );
    assert_eq!(protocol, [0]);

    let idle = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x02, // GET_IDLE
            w_value: 0,
            w_index: 0,
            w_length: 1,
        },
        1,
    );
    assert_eq!(idle, [7]);

    // Interrupt OUT endpoint should remain halted.
    assert_eq!(restored.handle_out(1, &[0x99]), UsbOutResult::Stall);

    // Pending input reports should survive snapshot/restore and be served in order.
    assert!(
        matches!(restored.handle_in(1, 8), UsbInResult::Data(data) if data == vec![0x11, 0x22])
    );
    assert!(
        matches!(restored.handle_in(1, 8), UsbInResult::Data(data) if data == vec![0x33, 0x44])
    );
    assert!(matches!(restored.handle_in(1, 8), UsbInResult::Nak));

    // Pending output reports should survive snapshot/restore so host integrations can drain them.
    assert_eq!(
        restored_handle.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 2,
            data: vec![0xAA, 0xBB],
        })
    );
    assert!(restored_handle.pop_output_report().is_none());

    // The guest-visible "last output report" state should still be preserved for GET_REPORT.
    let out_report = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x01,             // GET_REPORT
            w_value: (2u16 << 8) | 2u16, // Output report, ID 2
            w_index: 0,
            w_length: 3,
        },
        3,
    );
    assert_eq!(out_report, [2, 0xAA, 0xBB]);
}

#[derive(Clone)]
struct HubHandle(Rc<RefCell<UsbHubDevice>>);

impl HubHandle {
    fn new(hub: UsbHubDevice) -> Self {
        Self(Rc::new(RefCell::new(hub)))
    }

    fn inner(&self) -> Rc<RefCell<UsbHubDevice>> {
        self.0.clone()
    }
}

impl UsbDeviceModel for HubHandle {
    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        self.0
            .borrow_mut()
            .handle_control_request(setup, data_stage)
    }

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        self.0.borrow_mut().handle_in_transfer(ep, max_len)
    }

    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        self.0.borrow_mut().handle_out_transfer(ep, data)
    }

    fn tick_1ms(&mut self) {
        self.0.borrow_mut().tick_1ms();
    }

    fn set_suspended(&mut self, suspended: bool) {
        self.0.borrow_mut().set_suspended(suspended);
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        self.0.borrow_mut().poll_remote_wakeup()
    }
}

#[test]
fn hub_snapshot_roundtrip_preserves_port_reset_timer() {
    let hub_handle = HubHandle::new(UsbHubDevice::new());
    let hub_rc = hub_handle.inner();

    // Attach something so the port reports a connection.
    hub_rc.borrow_mut().attach(1, Box::new(DummyUsbDevice));

    let mut hub = AttachedUsbDevice::new(Box::new(hub_handle.clone()));

    control_no_data(
        &mut hub,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 3,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 1,      // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            bm_request_type: 0x02,
            b_request: 0x03, // SET_FEATURE
            w_value: 0,      // ENDPOINT_HALT
            w_index: 0x81,   // interrupt IN endpoint address
            w_length: 0,
        },
    );

    // Power and reset port 1.
    control_no_data(
        &mut hub,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: 8,      // PORT_POWER
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut hub,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: 4,      // PORT_RESET
            w_index: 1,
            w_length: 0,
        },
    );

    for _ in 0..10 {
        hub.tick_1ms();
    }

    let port_status = control_in(
        &mut hub,
        SetupPacket {
            bm_request_type: 0xA3,
            b_request: 0x00, // GET_STATUS
            w_value: 0,
            w_index: 1,
            w_length: 4,
        },
        4,
    );
    let st = u16::from_le_bytes([port_status[0], port_status[1]]);
    assert_ne!(st & (1 << 4), 0, "port reset should be active");

    let model_snapshot = hub_rc.borrow().save_state();
    let hub_snapshot = hub.save_state();

    let restored_handle = HubHandle::new(UsbHubDevice::new());
    let restored_rc = restored_handle.inner();
    restored_rc
        .borrow_mut()
        .load_state(&model_snapshot)
        .expect("hub snapshot restore should succeed");

    let mut restored = AttachedUsbDevice::new(Box::new(restored_handle));
    restored
        .load_state(&hub_snapshot)
        .expect("hub snapshot restore should succeed");

    assert_eq!(restored.address(), 3);

    let cfg = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x08, // GET_CONFIGURATION
            w_value: 0,
            w_index: 0,
            w_length: 1,
        },
        1,
    );
    assert_eq!(cfg, [1]);

    let status = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x00, // GET_STATUS
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
        2,
    );
    assert_eq!(status, [0x02, 0x00]);

    assert_eq!(restored.handle_in(1, 8), UsbInResult::Stall);

    // The reset countdown should survive restore: 40ms remaining.
    for _ in 0..39 {
        restored.tick_1ms();
    }
    let port_status = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA3,
            b_request: 0x00,
            w_value: 0,
            w_index: 1,
            w_length: 4,
        },
        4,
    );
    let st = u16::from_le_bytes([port_status[0], port_status[1]]);
    assert_ne!(st & (1 << 4), 0, "reset should still be active after 39ms");

    restored.tick_1ms();
    let port_status = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA3,
            b_request: 0x00,
            w_value: 0,
            w_index: 1,
            w_length: 4,
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
    let mut ctrl = UhciController::new();
    ctrl.hub_mut().attach(0, Box::new(DummyUsbDevice));

    let mut mem = TestMemory::new(0x4000);

    let fl_base = 0x1000;
    ctrl.io_write(REG_FRBASEADD, 4, fl_base);
    for i in 0..1024u32 {
        mem.write_u32(fl_base + i * 4, LINK_PTR_T);
    }

    // Start a port reset sequence (50ms).
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);

    // Run the controller against an empty schedule so FRNUM advances deterministically.
    let usbcmd = USBCMD_RUN | USBCMD_CF | USBCMD_MAXP;
    ctrl.io_write(REG_USBCMD, 2, usbcmd as u32);

    ctrl.io_write(REG_FRNUM, 2, 0x0123);
    ctrl.io_write(REG_SOFMOD, 1, 0x55);

    for _ in 0..10 {
        ctrl.tick_1ms(&mut mem);
    }

    let expected_frnum = ctrl.io_read(REG_FRNUM, 2);
    let expected_portsc1 = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(
        expected_portsc1 & PORTSC_PR,
        0,
        "reset should still be active"
    );

    let snapshot = ctrl.save_state();

    let mut restored = UhciController::new();
    restored
        .load_state(&snapshot)
        .expect("uhci snapshot restore should succeed");

    assert_eq!(restored.io_read(REG_FRBASEADD, 4), fl_base);
    assert_eq!(restored.io_read(REG_FRNUM, 2), expected_frnum);
    assert_eq!(restored.io_read(REG_SOFMOD, 1), 0x55);
    assert_eq!(restored.io_read(REG_PORTSC1, 2) as u16, expected_portsc1);

    // Root port connection must be preserved (CCS bit).
    let portsc1 = restored.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc1 & 0x0001,
        0,
        "root port connection must be preserved"
    );
    assert_eq!(
        portsc1 & PORTSC_PED,
        0,
        "root port should remain disabled during reset"
    );

    // Continue the reset timer: 40ms remaining.
    for _ in 0..39 {
        restored.tick_1ms(&mut mem);
    }
    let portsc1 = restored.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc1 & PORTSC_PR,
        0,
        "reset should still be active after 39ms"
    );

    restored.tick_1ms(&mut mem);
    let portsc1 = restored.io_read(REG_PORTSC1, 2) as u16;
    assert_eq!(portsc1 & PORTSC_PR, 0, "reset bit clears after 40ms");
    assert_ne!(
        portsc1 & PORTSC_PED,
        0,
        "port should be enabled after reset"
    );
}

#[test]
fn snapshot_device_id_mismatch_returns_error() {
    let dev = UsbHidPassthroughHandle::new(
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
    let snapshot = dev.save_state();
    let mut corrupted = snapshot.clone();
    corrupted[8..12].copy_from_slice(b"NOPE");

    let mut restored = UsbHidPassthroughHandle::new(
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
    let err = restored.load_state(&corrupted).unwrap_err();
    assert!(matches!(err, SnapshotError::DeviceIdMismatch { .. }));
}

#[test]
fn snapshot_major_version_mismatch_returns_error() {
    let dev = UsbHidPassthroughHandle::new(
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
    let snapshot = dev.save_state();
    let mut corrupted = snapshot.clone();
    corrupted[12..14].copy_from_slice(&2u16.to_le_bytes());

    let mut restored = UsbHidPassthroughHandle::new(
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
    let report_desc = sample_report_descriptor_input_2_bytes();
    let dev = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc.clone(),
        false,
        None,
        None,
        None,
    );
    let snapshot = dev.save_state();
    let mut corrupted = snapshot.clone();
    corrupted[14..16].copy_from_slice(&42u16.to_le_bytes());

    let mut restored = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc,
        false,
        None,
        None,
        None,
    );
    restored
        .load_state(&corrupted)
        .expect("minor version mismatch should be accepted");
}

#[test]
fn snapshot_unknown_fields_are_ignored() {
    let report_desc = sample_report_descriptor_input_2_bytes();
    let dev_handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc.clone(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(dev_handle.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    dev_handle.push_input_report(0, &[0x11, 0x22]);
    dev_handle.push_input_report(0, &[0x33, 0x44]);

    let snapshot = dev_handle.save_state();
    let mut extended = snapshot.clone();

    let tag = 999u16;
    let payload = [1u8, 2, 3, 4];
    extended.extend_from_slice(&tag.to_le_bytes());
    extended.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    extended.extend_from_slice(&payload);

    let mut restored = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc,
        false,
        None,
        None,
        None,
    );
    restored
        .load_state(&extended)
        .expect("unknown TLV tags should be ignored");
    assert!(restored.configured());

    match restored.handle_in_transfer(0x81, 8) {
        UsbInResult::Data(data) => assert_eq!(data, vec![0x11, 0x22]),
        other => panic!("expected first report data, got {other:?}"),
    }
    match restored.handle_in_transfer(0x81, 8) {
        UsbInResult::Data(data) => assert_eq!(data, vec![0x33, 0x44]),
        other => panic!("expected second report data, got {other:?}"),
    }
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
    let model = UsbWebUsbPassthroughDevice::new();
    let mut dev = AttachedUsbDevice::new(Box::new(model.clone()));

    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 12,
            w_index: 0,
            w_length: 0,
        }),
        UsbOutResult::Ack
    );
    assert_eq!(dev.address(), 0);

    let model_snapshot = model.save_state();
    let dev_snapshot = dev.save_state();

    let mut restored_model = UsbWebUsbPassthroughDevice::new();
    restored_model
        .load_state(&model_snapshot)
        .expect("webusb snapshot restore should succeed");

    let mut restored = AttachedUsbDevice::new(Box::new(restored_model.clone()));
    restored
        .load_state(&dev_snapshot)
        .expect("webusb snapshot restore should succeed");

    assert_eq!(restored.address(), 0);

    // Status stage for SET_ADDRESS is an IN ZLP.
    assert!(matches!(restored.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()));
    assert_eq!(restored.address(), 12);
}

#[test]
fn webusb_passthrough_device_snapshot_requeues_control_in_action() {
    let model = UsbWebUsbPassthroughDevice::new();
    let mut dev = AttachedUsbDevice::new(Box::new(model.clone()));

    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06, // GET_DESCRIPTOR
        w_value: 0x0100,
        w_index: 0,
        w_length: 4,
    };
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let actions = model.drain_actions();
    assert_eq!(actions.len(), 1);
    let id1 = match actions[0] {
        UsbHostAction::ControlIn {
            id,
            setup: host_setup,
        } => {
            assert_eq!(
                host_setup,
                HostSetupPacket {
                    bm_request_type: setup.bm_request_type,
                    b_request: setup.b_request,
                    w_value: setup.w_value,
                    w_index: setup.w_index,
                    w_length: setup.w_length,
                }
            );
            id
        }
        ref other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(id1, 1);

    let model_snapshot = model.save_state();
    let dev_snapshot = dev.save_state();

    let mut restored_model = UsbWebUsbPassthroughDevice::new();
    restored_model
        .load_state(&model_snapshot)
        .expect("webusb snapshot restore should succeed");

    let mut restored = AttachedUsbDevice::new(Box::new(restored_model.clone()));
    restored
        .load_state(&dev_snapshot)
        .expect("webusb snapshot restore should succeed");

    // Host actions are backed by JS Promises in the browser runtime; after restoring a VM snapshot
    // they cannot be resumed. Host integrations should clear host-side inflight state so the guest
    // TD retries can re-emit new host actions.
    restored_model.reset_host_state_for_restore();

    // First poll should NAK and re-queue a fresh host action.
    assert_eq!(restored.handle_in(0, 4), UsbInResult::Nak);

    let actions = restored_model.drain_actions();
    assert_eq!(actions.len(), 1);
    let id2 = match actions[0] {
        UsbHostAction::ControlIn {
            id,
            setup: host_setup,
        } => {
            assert_eq!(
                host_setup,
                HostSetupPacket {
                    bm_request_type: setup.bm_request_type,
                    b_request: setup.b_request,
                    w_value: setup.w_value,
                    w_index: setup.w_index,
                    w_length: setup.w_length,
                }
            );
            id
        }
        ref other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(id2, 2);

    restored_model.push_completion(UsbHostCompletion::ControlIn {
        id: id2,
        result: UsbHostCompletionIn::Success {
            data: vec![9, 8, 7, 6],
        },
    });

    assert!(
        matches!(restored.handle_in(0, 4), UsbInResult::Data(data) if data == vec![9, 8, 7, 6])
    );

    // Status stage for control-IN is an OUT ZLP.
    assert_eq!(restored.handle_out(0, &[]), UsbOutResult::Ack);
}
