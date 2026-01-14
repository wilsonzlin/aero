use aero_usb::hid::{UsbHidKeyboardHandle, UsbHidMouseHandle};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

const INTERRUPT_IN_EP: u8 = 0x81;

fn configure(dev: &mut impl UsbDeviceModel) {
    assert_eq!(
        dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack
    );
}

fn set_idle(dev: &mut impl UsbDeviceModel, idle_rate: u8) {
    assert_eq!(
        dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x21, // HostToDevice | Class | Interface
                b_request: 0x0a,       // SET_IDLE
                w_value: (idle_rate as u16) << 8,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack
    );
}

fn poll_interrupt_in(dev: &mut impl UsbDeviceModel, max_len: usize) -> Option<Vec<u8>> {
    match dev.handle_in_transfer(INTERRUPT_IN_EP, max_len) {
        UsbInResult::Data(data) => Some(data),
        UsbInResult::Nak => None,
        UsbInResult::Stall => panic!("unexpected STALL on interrupt IN"),
        UsbInResult::Timeout => panic!("unexpected TIMEOUT on interrupt IN"),
    }
}

#[test]
fn keyboard_idle_rate_reemits_current_report() {
    let mut kb = UsbHidKeyboardHandle::new();
    configure(&mut kb);
    set_idle(&mut kb, 2); // 8ms

    // Generate an initial non-empty report.
    kb.key_event(0x04, true); // 'a'
    assert_eq!(
        poll_interrupt_in(&mut kb, 8).unwrap(),
        vec![0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );
    assert_eq!(poll_interrupt_in(&mut kb, 8), None);

    // Poll every 1ms; expect the same report exactly every 8ms.
    for _ in 0..7 {
        kb.tick_1ms();
        assert_eq!(poll_interrupt_in(&mut kb, 8), None);
    }
    kb.tick_1ms();
    assert_eq!(
        poll_interrupt_in(&mut kb, 8).unwrap(),
        vec![0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );

    for _ in 0..7 {
        kb.tick_1ms();
        assert_eq!(poll_interrupt_in(&mut kb, 8), None);
    }
    kb.tick_1ms();
    assert_eq!(
        poll_interrupt_in(&mut kb, 8).unwrap(),
        vec![0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );
}

#[test]
fn keyboard_idle_rate_zero_does_not_reemit_without_changes() {
    let mut kb = UsbHidKeyboardHandle::new();
    configure(&mut kb);
    set_idle(&mut kb, 0);

    kb.key_event(0x04, true); // 'a'
    assert_eq!(
        poll_interrupt_in(&mut kb, 8).unwrap(),
        vec![0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );

    // Hold key; no further reports should be produced when idle_rate=0.
    for _ in 0..100 {
        kb.tick_1ms();
        assert_eq!(poll_interrupt_in(&mut kb, 8), None);
    }

    kb.key_event(0x04, false);
    assert_eq!(poll_interrupt_in(&mut kb, 8).unwrap(), vec![0; 8]);
}

#[test]
fn mouse_idle_rate_reemits_current_report() {
    let mut mouse = UsbHidMouseHandle::new();
    configure(&mut mouse);
    set_idle(&mut mouse, 2); // 8ms

    mouse.button_event(0x01, true); // left button
    assert_eq!(
        poll_interrupt_in(&mut mouse, 5).unwrap(),
        vec![0x01, 0x00, 0x00, 0x00, 0x00]
    );
    assert_eq!(poll_interrupt_in(&mut mouse, 5), None);

    for _ in 0..7 {
        mouse.tick_1ms();
        assert_eq!(poll_interrupt_in(&mut mouse, 5), None);
    }
    mouse.tick_1ms();
    assert_eq!(
        poll_interrupt_in(&mut mouse, 5).unwrap(),
        vec![0x01, 0x00, 0x00, 0x00, 0x00]
    );
}

#[test]
fn mouse_idle_rate_zero_does_not_reemit_without_changes() {
    let mut mouse = UsbHidMouseHandle::new();
    configure(&mut mouse);
    set_idle(&mut mouse, 0);

    mouse.button_event(0x01, true);
    assert_eq!(
        poll_interrupt_in(&mut mouse, 5).unwrap(),
        vec![0x01, 0x00, 0x00, 0x00, 0x00]
    );

    for _ in 0..100 {
        mouse.tick_1ms();
        assert_eq!(poll_interrupt_in(&mut mouse, 5), None);
    }

    mouse.button_event(0x01, false);
    assert_eq!(
        poll_interrupt_in(&mut mouse, 5).unwrap(),
        vec![0x00, 0x00, 0x00, 0x00, 0x00]
    );
}
