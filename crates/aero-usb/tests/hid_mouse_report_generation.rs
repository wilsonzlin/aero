use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotVersion, SnapshotWriter};
use aero_usb::hid::UsbHidMouse;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

const INTERRUPT_IN_EP: u8 = 0x81;

fn configure_mouse(mouse: &mut UsbHidMouse) {
    assert_eq!(
        mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00, // HostToDevice | Standard | Device
                b_request: 0x09,       // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack
    );
}

fn set_protocol(mouse: &mut UsbHidMouse, protocol: u16) {
    assert_eq!(
        mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x21, // HostToDevice | Class | Interface
                b_request: 0x0b,       // SET_PROTOCOL
                w_value: protocol,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack
    );
}

fn get_report(mouse: &mut UsbHidMouse, w_length: u16) -> Vec<u8> {
    match mouse.handle_control_request(
        SetupPacket {
            bm_request_type: 0xA1, // DeviceToHost | Class | Interface
            b_request: 0x01,       // GET_REPORT
            w_value: 1u16 << 8,    // Input report, ID 0
            w_index: 0,
            w_length,
        },
        None,
    ) {
        ControlResponse::Data(data) => data,
        other => panic!("expected GET_REPORT to return data, got {other:?}"),
    }
}

fn poll_interrupt_in(mouse: &mut UsbHidMouse) -> Option<Vec<u8>> {
    match mouse.handle_in_transfer(INTERRUPT_IN_EP, 5) {
        UsbInResult::Data(data) => Some(data),
        UsbInResult::Nak => None,
        UsbInResult::Stall => panic!("unexpected STALL on interrupt IN"),
        UsbInResult::Timeout => panic!("unexpected TIMEOUT on interrupt IN"),
    }
}

fn drain_interrupt_reports(mouse: &mut UsbHidMouse) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(data) = poll_interrupt_in(mouse) {
        out.push(data);
    }
    out
}

fn parse_report(data: &[u8]) -> (u8, i8, i8, Option<i8>, Option<i8>) {
    match data {
        [buttons, x, y] => (*buttons, *x as i8, *y as i8, None, None),
        [buttons, x, y, wheel] => (*buttons, *x as i8, *y as i8, Some(*wheel as i8), Some(0)),
        [buttons, x, y, wheel, hwheel] => (
            *buttons,
            *x as i8,
            *y as i8,
            Some(*wheel as i8),
            Some(*hwheel as i8),
        ),
        other => panic!("unexpected HID mouse report length: {}", other.len()),
    }
}

#[test]
fn mouse_motion_splits_deltas_with_saturation_and_remainder() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    // Verify we saturate at ±127 (not i8::MIN=-128) and carry the remainder into a follow-up
    // report.
    mouse.movement(128, -128);

    let reports = drain_interrupt_reports(&mut mouse);
    assert_eq!(reports.len(), 2);

    assert_eq!(
        parse_report(&reports[0]),
        (0x00, 127, -127, Some(0), Some(0))
    );
    assert_eq!(parse_report(&reports[1]), (0x00, 1, -1, Some(0), Some(0)));
}

#[test]
fn mouse_motion_splits_large_dx_dy_into_multiple_reports() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    mouse.movement(300, -300);

    let reports = drain_interrupt_reports(&mut mouse);
    assert_eq!(reports.len(), 3);

    assert_eq!(
        parse_report(&reports[0]),
        (0x00, 127, -127, Some(0), Some(0))
    );
    assert_eq!(
        parse_report(&reports[1]),
        (0x00, 127, -127, Some(0), Some(0))
    );
    assert_eq!(parse_report(&reports[2]), (0x00, 46, -46, Some(0), Some(0)));
}

#[test]
fn mouse_motion_bounds_packet_generation_for_extreme_deltas() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    // Chosen to require >128 reports (127*200 + 1). Without a work cap this would produce 201
    // reports and rely on queue eviction; with a cap we should not reach the final remainder.
    mouse.movement(127 * 200 + 1, 0);

    let reports = drain_interrupt_reports(&mut mouse);
    assert_eq!(
        reports.len(),
        128,
        "expected extreme motion to be capped to the pending report limit"
    );

    let last = reports.last().expect("expected at least one report");
    let (_buttons, x, _y, _wheel, _hwheel) = parse_report(last);
    assert_eq!(
        x, 127,
        "expected capped motion to drop the final remainder report"
    );
}

#[test]
fn mouse_wheel_splits_deltas_with_saturation_and_remainder() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    mouse.wheel(128);
    mouse.wheel(-128);

    let reports = drain_interrupt_reports(&mut mouse);
    assert_eq!(reports.len(), 4);

    assert_eq!(parse_report(&reports[0]), (0x00, 0, 0, Some(127), Some(0)));
    assert_eq!(parse_report(&reports[1]), (0x00, 0, 0, Some(1), Some(0)));
    assert_eq!(parse_report(&reports[2]), (0x00, 0, 0, Some(-127), Some(0)));
    assert_eq!(parse_report(&reports[3]), (0x00, 0, 0, Some(-1), Some(0)));
}

#[test]
fn mouse_hwheel_splits_deltas_with_saturation_and_remainder() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    mouse.hwheel(128);
    mouse.hwheel(-128);

    let reports = drain_interrupt_reports(&mut mouse);
    assert_eq!(reports.len(), 4);

    assert_eq!(parse_report(&reports[0]), (0x00, 0, 0, Some(0), Some(127)));
    assert_eq!(parse_report(&reports[1]), (0x00, 0, 0, Some(0), Some(1)));
    assert_eq!(parse_report(&reports[2]), (0x00, 0, 0, Some(0), Some(-127)));
    assert_eq!(parse_report(&reports[3]), (0x00, 0, 0, Some(0), Some(-1)));
}

#[test]
fn mouse_wheel2_emits_combined_wheel_axes_in_single_reports() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    mouse.wheel2(1, -2);

    let reports = drain_interrupt_reports(&mut mouse);
    assert_eq!(reports.len(), 1);
    assert_eq!(parse_report(&reports[0]), (0x00, 0, 0, Some(1), Some(-2)));
}

#[test]
fn mouse_wheel2_splits_large_deltas_into_multiple_reports() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    mouse.wheel2(300, -300);

    let reports = drain_interrupt_reports(&mut mouse);
    assert_eq!(reports.len(), 3);

    assert_eq!(
        parse_report(&reports[0]),
        (0x00, 0, 0, Some(127), Some(-127))
    );
    assert_eq!(
        parse_report(&reports[1]),
        (0x00, 0, 0, Some(127), Some(-127))
    );
    assert_eq!(parse_report(&reports[2]), (0x00, 0, 0, Some(46), Some(-46)));
}

#[test]
fn configuration_does_not_replay_unconfigured_wheel_events() {
    let mut mouse = UsbHidMouse::new();

    mouse.wheel2(5, 7);
    assert!(poll_interrupt_in(&mut mouse).is_none());

    configure_mouse(&mut mouse);
    assert!(
        poll_interrupt_in(&mut mouse).is_none(),
        "wheel input injected before configuration should not be replayed after configuration"
    );
}

#[test]
fn mouse_wheel_is_ignored_in_boot_protocol() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    // Boot protocol mice have no wheel/hwheel axes. We intentionally drop scroll events rather than
    // enqueueing no-op packets that could evict real motion/button reports.
    set_protocol(&mut mouse, 0);
    mouse.wheel2(1, -2);

    assert!(
        poll_interrupt_in(&mut mouse).is_none(),
        "expected boot protocol wheel input to produce no interrupt report"
    );
}

#[test]
fn boot_protocol_wheel_still_triggers_remote_wakeup() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    // Enable remote wakeup via SET_FEATURE(DEVICE_REMOTE_WAKEUP).
    assert_eq!(
        mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00, // HostToDevice | Standard | Device
                b_request: 0x03,       // SET_FEATURE
                w_value: 1,            // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack
    );

    // Enter suspend and inject a scroll event while in boot protocol.
    mouse.set_suspended(true);
    set_protocol(&mut mouse, 0);
    mouse.wheel(1);

    assert!(
        mouse.poll_remote_wakeup(),
        "expected scroll input to set remote wakeup pending even in boot protocol"
    );
    assert!(
        !mouse.poll_remote_wakeup(),
        "remote wakeup should be edge-triggered"
    );
}

#[test]
fn mouse_buttons_4_and_5_are_reported_in_report_protocol_but_masked_in_boot_protocol() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    // Default protocol is Report; button bits 4/5 should be visible in interrupt reports.
    mouse.button_event(0x01, true); // left
    mouse.button_event(0x08, true); // back (button 4)
    mouse.button_event(0x10, true); // forward (button 5)

    let reports = drain_interrupt_reports(&mut mouse);
    let last = reports.last().expect("expected button reports");
    assert_eq!(
        last.len(),
        5,
        "report protocol mouse report should be 5 bytes"
    );
    assert_eq!(parse_report(last), (0x19, 0, 0, Some(0), Some(0)));

    // Switching to boot protocol must mask the extra button bits. We trigger a report via motion
    // while buttons 4/5 remain held.
    set_protocol(&mut mouse, 0);
    mouse.movement(1, 0);

    let boot_report = poll_interrupt_in(&mut mouse).expect("expected boot protocol report");
    assert_eq!(boot_report.len(), 3, "boot mouse reports are 3 bytes");
    assert_eq!(parse_report(&boot_report), (0x01, 1, 0, None, None));

    // GET_REPORT should also reflect boot protocol formatting + masking.
    let current = get_report(&mut mouse, 64);
    assert_eq!(current.len(), 3);
    assert_eq!(parse_report(&current), (0x01, 0, 0, None, None));
}

#[test]
fn snapshot_restore_preserves_pending_queue_and_button_state() {
    let mut mouse = UsbHidMouse::new();
    configure_mouse(&mut mouse);

    mouse.button_event(0x01, true); // left
    mouse.button_event(0x10, true); // forward (button 5)
    mouse.movement(128, -128);
    mouse.wheel(128);

    // Pop one report so there is a non-trivial pending queue to preserve.
    let _ = poll_interrupt_in(&mut mouse).expect("expected at least one report");

    let snapshot = mouse.save_state();

    let mut restored = UsbHidMouse::new();
    restored.load_state(&snapshot).unwrap();

    // Remaining reports must be identical and in the same order after restore.
    let expected_remaining = drain_interrupt_reports(&mut mouse);
    let restored_remaining = drain_interrupt_reports(&mut restored);
    assert_eq!(restored_remaining, expected_remaining);

    // Button state must persist across restore (including button 5 while in report protocol).
    let report = get_report(&mut restored, 64);
    assert_eq!(report.len(), 5);
    assert_eq!(parse_report(&report), (0x11, 0, 0, Some(0), Some(0)));

    // And the button state should affect subsequently-generated reports.
    restored.movement(1, 0);
    let moved = poll_interrupt_in(&mut restored).expect("expected movement report");
    assert_eq!(parse_report(&moved), (0x11, 1, 0, Some(0), Some(0)));
}

#[test]
fn snapshot_load_accepts_v1_1_reports_without_hwheel_byte() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_PROTOCOL: u16 = 8;
    const TAG_PENDING_REPORTS: u16 = 13;

    // Version 1.1 snapshots encode mouse reports as 4 bytes:
    // (buttons, x, y, wheel). Version 1.2 appends an `hwheel` byte.
    let mut w = SnapshotWriter::new(
        <UsbHidMouse as IoSnapshot>::DEVICE_ID,
        SnapshotVersion::new(1, 1),
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_u8(TAG_PROTOCOL, 1); // Report protocol.
    w.field_bytes(
        TAG_PENDING_REPORTS,
        Encoder::new()
            .vec_bytes(&[vec![0x03, 1u8, 2u8, 3u8]])
            .finish(),
    );
    let snap = w.finish();

    let mut mouse = UsbHidMouse::new();
    mouse.load_state(&snap).unwrap();

    let report = match mouse.handle_in_transfer(INTERRUPT_IN_EP, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored report data, got {other:?}"),
    };
    assert_eq!(
        report,
        vec![0x03, 1, 2, 3, 0],
        "expected missing v1.1 hwheel byte to restore as 0"
    );
    assert!(matches!(
        mouse.handle_in_transfer(INTERRUPT_IN_EP, 5),
        UsbInResult::Nak
    ));
}
