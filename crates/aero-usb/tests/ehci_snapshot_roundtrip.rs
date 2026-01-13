use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::device::AttachedUsbDevice;
use aero_usb::ehci::regs::{
    PORTSC_CCS, PORTSC_PED, PORTSC_PP, PORTSC_PR, REG_ASYNCLISTADDR, REG_CONFIGFLAG, REG_FRINDEX,
    REG_PERIODICLISTBASE, REG_PORTSC_BASE, REG_USBCMD, REG_USBLEGCTLSTS, REG_USBLEGSUP,
    USBLEGSUP_OS_SEM, USBCMD_RS,
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
    assert_eq!(restored.mmio_read(REG_USBLEGCTLSTS, 4), expected_usblegctlsts);

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
    let dev = restored
        .hub_mut()
        .device_mut_for_address(0)
        .expect("device should be reachable after reset");

    control_no_data(
        dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 5,
            w_index: 0,
            w_length: 0,
        },
    );

    let desc = control_in(
        dev,
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
