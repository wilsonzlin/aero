use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::state::{SnapshotError, SnapshotReader, SnapshotWriter};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::ehci::regs::{
    PORTSC_CCS, PORTSC_PED, PORTSC_PO, PORTSC_PP, PORTSC_PR, REG_ASYNCLISTADDR, REG_CONFIGFLAG,
    REG_FRINDEX, REG_PERIODICLISTBASE, REG_PORTSC_BASE, REG_USBCMD, REG_USBLEGCTLSTS,
    REG_USBLEGSUP, USBCMD_RS, USBLEGSUP_OS_SEM,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

mod util;

use util::TestMemory;

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

#[test]
fn ehci_snapshot_roundtrip_preserves_regs_port_timer_and_topology() {
    let mut ctrl = EhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));

    let mut mem = TestMemory::new(0x2000);

    // Program schedule base registers and run the controller so FRINDEX advances.
    ctrl.mmio_write(REG_PERIODICLISTBASE, 4, 0x1000);
    ctrl.mmio_write(REG_ASYNCLISTADDR, 4, 0x2000);
    ctrl.mmio_write(REG_CONFIGFLAG, 4, 1);
    ctrl.mmio_write(REG_USBCMD, 4, USBCMD_RS);

    ctrl.mmio_write(REG_FRINDEX, 4, 0x0123);
    // Exercise EHCI legacy handoff registers and ensure they snapshot/restore.
    ctrl.mmio_write(REG_USBLEGSUP, 4, USBLEGSUP_OS_SEM);
    ctrl.mmio_write(REG_USBLEGCTLSTS, 4, 0x1122_3344);

    // Start a port reset sequence (50ms).
    ctrl.mmio_write(REG_PORTSC_BASE, 4, PORTSC_PP);
    ctrl.mmio_write(REG_PORTSC_BASE, 4, PORTSC_PP | PORTSC_PR);

    for _ in 0..10 {
        ctrl.tick_1ms(&mut mem);
    }

    let expected_frindex = ctrl.mmio_read(REG_FRINDEX, 4);
    let expected_portsc1 = ctrl.mmio_read(REG_PORTSC_BASE, 4);
    let expected_usblegsup = ctrl.mmio_read(REG_USBLEGSUP, 4);
    let expected_usblegctlsts = ctrl.mmio_read(REG_USBLEGCTLSTS, 4);
    assert_ne!(
        expected_portsc1 & PORTSC_PR,
        0,
        "reset should still be active"
    );

    let snapshot = ctrl.save_state();

    // Restore into a fresh controller with no pre-attached devices so we exercise topology
    // reconstruction from nested `ADEV` snapshots.
    let mut restored = EhciController::new();
    restored
        .load_state(&snapshot)
        .expect("ehci snapshot restore should succeed");

    assert_eq!(restored.mmio_read(REG_PERIODICLISTBASE, 4), 0x1000);
    assert_eq!(restored.mmio_read(REG_ASYNCLISTADDR, 4), 0x2000);
    assert_eq!(restored.mmio_read(REG_CONFIGFLAG, 4), 1);
    assert_eq!(restored.mmio_read(REG_FRINDEX, 4), expected_frindex);
    assert_eq!(restored.mmio_read(REG_PORTSC_BASE, 4), expected_portsc1);
    assert_eq!(restored.mmio_read(REG_USBLEGSUP, 4), expected_usblegsup);
    assert_eq!(
        restored.mmio_read(REG_USBLEGCTLSTS, 4),
        expected_usblegctlsts
    );

    // Root port connection must be preserved (CCS bit).
    let portsc1 = restored.mmio_read(REG_PORTSC_BASE, 4);
    assert_ne!(
        portsc1 & PORTSC_CCS,
        0,
        "root port connection must be preserved"
    );

    // Continue the reset timer: 40ms remaining.
    for _ in 0..39 {
        restored.tick_1ms(&mut mem);
    }
    let portsc1 = restored.mmio_read(REG_PORTSC_BASE, 4);
    assert_ne!(
        portsc1 & PORTSC_PR,
        0,
        "reset should still be active after 39ms"
    );

    restored.tick_1ms(&mut mem);
    let portsc1 = restored.mmio_read(REG_PORTSC_BASE, 4);
    assert_eq!(portsc1 & PORTSC_PR, 0, "reset bit clears after 40ms");
    assert_ne!(
        portsc1 & PORTSC_PED,
        0,
        "port should be enabled after reset"
    );

    // The device should be reachable on the enabled port; verify it responds to a standard control
    // request after restore.
    let mut dev = restored
        .hub_mut()
        .device_mut_for_address(0)
        .expect("device should be reachable after reset");

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

    let desc = control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06, // GET_DESCRIPTOR
            w_value: 0x0100, // Device descriptor
            w_index: 0,
            w_length: 18,
        },
        18,
    );
    assert_eq!(desc[0], 18, "device descriptor length must be 18");
    assert_eq!(desc[1], 0x01, "descriptor type must be DEVICE");
}

#[test]
fn ehci_snapshot_roundtrip_preserves_port_owner_unreachability() {
    let mut ctrl = EhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));

    let mut mem = TestMemory::new(0x2000);

    // Claim ports for EHCI so the port starts owned by EHCI, then explicitly hand the first port to
    // a companion controller via PORTSC.PORT_OWNER. The device should remain physically connected
    // but unreachable from the EHCI controller.
    ctrl.mmio_write(REG_CONFIGFLAG, 4, 1);
    ctrl.mmio_write(REG_PORTSC_BASE, 4, PORTSC_PP | PORTSC_PO);
    let portsc = ctrl.mmio_read(REG_PORTSC_BASE, 4);
    assert_ne!(portsc & PORTSC_PO, 0, "expected PORT_OWNER to be set");

    let snapshot = ctrl.save_state();

    let mut restored = EhciController::new();
    restored
        .load_state(&snapshot)
        .expect("ehci snapshot restore should succeed");

    let portsc = restored.mmio_read(REG_PORTSC_BASE, 4);
    assert_ne!(
        portsc & PORTSC_PO,
        0,
        "PORT_OWNER must be preserved across snapshot/restore"
    );
    assert!(
        restored.hub().port_device(0).is_some(),
        "device instance should be restored even when port is owned by companion"
    );
    assert!(
        restored.hub_mut().device_mut_for_address(0).is_none(),
        "device must be unreachable from EHCI when PORT_OWNER is set"
    );

    // Clear PORT_OWNER, perform a port reset, and ensure the device becomes reachable again.
    restored.mmio_write(REG_PORTSC_BASE, 4, PORTSC_PP);
    restored.mmio_write(REG_PORTSC_BASE, 4, PORTSC_PP | PORTSC_PR);
    for _ in 0..50 {
        restored.tick_1ms(&mut mem);
    }
    assert!(
        restored.hub_mut().device_mut_for_address(0).is_some(),
        "device should be reachable once PORT_OWNER is cleared and port reset completes"
    );
}

#[test]
fn ehci_snapshot_restore_rejects_oversized_nested_usb_device_snapshots() {
    // Construct a valid EHCI snapshot, then replace the first port's nested ADEV length with an
    // oversized value so `load_state` errors out before attempting to allocate/copy bytes.
    let ctrl = EhciController::new();
    let snapshot = ctrl.save_state();

    const TAG_ROOT_HUB_PORTS: u16 = 8;
    const MAX_USB_DEVICE_SNAPSHOT_BYTES: u32 = 4 * 1024 * 1024;

    let r = SnapshotReader::parse(&snapshot, EhciController::DEVICE_ID).unwrap();
    let Some(root_ports_bytes) = r.bytes(TAG_ROOT_HUB_PORTS) else {
        panic!("expected ROOT_HUB_PORTS field in EHCI snapshot");
    };

    let mut d = Decoder::new(root_ports_bytes);
    let mut port_records = d.vec_bytes().unwrap();
    d.finish().unwrap();
    assert!(
        !port_records.is_empty(),
        "expected at least one root port record"
    );

    // Replace port 0 record with one that declares an oversized device snapshot length.
    let oversize_len = MAX_USB_DEVICE_SNAPSHOT_BYTES + 1;
    port_records[0] = Encoder::new()
        .bool(false) // connected
        .bool(false) // connect_change
        .bool(false) // enabled
        .bool(false) // enable_change
        .bool(false) // over_current
        .bool(false) // over_current_change
        .bool(false) // reset
        .u8(0) // reset_countdown_ms
        .bool(false) // suspended
        .bool(false) // resuming
        .u8(0) // resume_countdown_ms
        .bool(false) // powered
        .bool(false) // port_owner
        .bool(true) // has_device_state
        .u32(oversize_len)
        .finish();

    let modified_root_ports = Encoder::new().vec_bytes(&port_records).finish();

    let mut w = SnapshotWriter::new(EhciController::DEVICE_ID, EhciController::DEVICE_VERSION);
    for (tag, bytes) in r.iter_fields() {
        if tag == TAG_ROOT_HUB_PORTS {
            w.field_bytes(tag, modified_root_ports.clone());
        } else {
            w.field_bytes(tag, bytes.to_vec());
        }
    }
    let bad_snapshot = w.finish();

    let mut restored = EhciController::new();
    let err = restored.load_state(&bad_snapshot).unwrap_err();
    match err {
        SnapshotError::InvalidFieldEncoding(msg) => {
            assert_eq!(msg, "usb device snapshot too large");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn ehci_snapshot_restore_detaches_pre_attached_devices_when_snapshot_has_none() {
    // Snapshot an empty topology (no devices), then restore into a controller that has a device
    // pre-attached. The snapshot should be authoritative and must detach the device.
    let empty = EhciController::new();
    let snapshot = empty.save_state();

    let mut ctrl = EhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));
    assert!(
        ctrl.hub().port_device(0).is_some(),
        "precondition: device should be attached"
    );

    ctrl.load_state(&snapshot)
        .expect("ehci snapshot restore should succeed");

    assert!(
        ctrl.hub().port_device(0).is_none(),
        "restored snapshot should detach pre-attached devices when snapshot has none"
    );
    let portsc = ctrl.mmio_read(REG_PORTSC_BASE, 4);
    assert_eq!(portsc & PORTSC_CCS, 0, "CCS should be cleared");
}
