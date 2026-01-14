use aero_io_snapshot::io::state::{IoSnapshot, SnapshotWriter};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::{
    UsbCompositeHidInputHandle, UsbHidKeyboardHandle, KEYBOARD_LED_CAPS_LOCK, KEYBOARD_LED_COMPOSE,
    KEYBOARD_LED_KANA, KEYBOARD_LED_MASK, KEYBOARD_LED_NUM_LOCK, KEYBOARD_LED_SCROLL_LOCK,
};
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    for _ in 0..16 {
        match dev.handle_in(0, 0) {
            UsbInResult::Data(data) => {
                assert!(data.is_empty(), "expected ZLP for status stage");
                return;
            }
            UsbInResult::Nak => continue,
            UsbInResult::Stall => panic!("unexpected STALL during control transfer"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control transfer"),
        }
    }
    panic!("timed out waiting for control transfer status stage");
}

fn control_out_data(dev: &mut AttachedUsbDevice, setup: SetupPacket, data: &[u8]) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert_eq!(dev.handle_out(0, data), UsbOutResult::Ack);

    // Status stage for control-OUT is an IN ZLP.
    for _ in 0..16 {
        match dev.handle_in(0, 0) {
            UsbInResult::Data(resp) => {
                assert!(resp.is_empty(), "expected ZLP for status stage");
                return;
            }
            UsbInResult::Nak => continue,
            UsbInResult::Stall => panic!("unexpected STALL during control transfer status stage"),
            UsbInResult::Timeout => {
                panic!("unexpected TIMEOUT during control transfer status stage")
            }
        }
    }
    panic!("timed out waiting for control transfer status stage");
}

#[test]
fn hid_keyboard_set_report_updates_handle_leds_and_snapshot_roundtrip_preserves_them() {
    let kb = UsbHidKeyboardHandle::new();
    let mut dev = AttachedUsbDevice::new(Box::new(kb.clone()));

    assert_eq!(kb.leds(), 0);

    // Basic enumeration/configuration so the guest can send HID class requests.
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

    // SET_REPORT(Output) to set boot keyboard LEDs: NumLock | CapsLock | ScrollLock | Compose | Kana.
    // Send high bits as well; devices should ignore the padding bits and only track the 5 LED
    // usages defined by the report descriptor.
    let leds = 0xff;
    let expected_leds = KEYBOARD_LED_NUM_LOCK
        | KEYBOARD_LED_CAPS_LOCK
        | KEYBOARD_LED_SCROLL_LOCK
        | KEYBOARD_LED_COMPOSE
        | KEYBOARD_LED_KANA;
    control_out_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x09,    // SET_REPORT
            w_value: 2u16 << 8, // Output report, ID 0
            w_index: 0,
            w_length: 1,
        },
        &[leds],
    );

    assert_eq!(kb.leds(), expected_leds);

    // Snapshot both the device wrapper and the host-visible handle (pre-attached model case).
    let dev_snapshot = dev.save_state();
    let model_snapshot = kb.save_state();

    let mut restored_kb = UsbHidKeyboardHandle::new();
    restored_kb.load_state(&model_snapshot).unwrap();
    assert_eq!(restored_kb.leds(), expected_leds);

    let kb_from_dev = UsbHidKeyboardHandle::new();
    let mut restored_dev = AttachedUsbDevice::new(Box::new(kb_from_dev.clone()));
    restored_dev.load_state(&dev_snapshot).unwrap();
    assert_eq!(kb_from_dev.leds(), expected_leds);
}

#[test]
fn hid_composite_keyboard_set_report_updates_handle_leds_and_snapshot_roundtrip_preserves_them() {
    let hid = UsbCompositeHidInputHandle::new();
    let mut dev = AttachedUsbDevice::new(Box::new(hid.clone()));

    assert_eq!(hid.keyboard_leds(), 0);

    // Basic enumeration/configuration so the guest can send HID class requests.
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

    // SET_REPORT(Output) to set boot keyboard LEDs: NumLock | CapsLock | ScrollLock | Compose | Kana.
    // Send high bits as well; devices should ignore the padding bits and only track the 5 LED
    // usages defined by the report descriptor.
    let leds = 0xff;
    let expected_leds = KEYBOARD_LED_MASK;
    control_out_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x09,    // SET_REPORT
            w_value: 2u16 << 8, // Output report, ID 0
            w_index: 0,         // interface 0 (keyboard)
            w_length: 1,
        },
        &[leds],
    );

    assert_eq!(hid.keyboard_leds(), expected_leds);

    let dev_snapshot = dev.save_state();
    let model_snapshot = hid.save_state();

    let mut restored_hid = UsbCompositeHidInputHandle::new();
    restored_hid.load_state(&model_snapshot).unwrap();
    assert_eq!(restored_hid.keyboard_leds(), expected_leds);

    let hid_from_dev = UsbCompositeHidInputHandle::new();
    let mut restored_dev = AttachedUsbDevice::new(Box::new(hid_from_dev.clone()));
    restored_dev.load_state(&dev_snapshot).unwrap();
    assert_eq!(hid_from_dev.keyboard_leds(), expected_leds);
}

#[test]
fn hid_keyboard_snapshot_load_masks_led_padding_bits() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_LEDS: u16 = 9;

    let mut w = SnapshotWriter::new(
        <UsbHidKeyboardHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidKeyboardHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_LEDS, 0xff);
    let snap = w.finish();

    let mut kb = UsbHidKeyboardHandle::new();
    kb.load_state(&snap).unwrap();
    assert_eq!(kb.leds(), KEYBOARD_LED_MASK);
}

#[test]
fn hid_composite_snapshot_load_masks_led_padding_bits() {
    const TAG_KBD_LEDS: u16 = 12;

    let mut w = SnapshotWriter::new(
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_ID,
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_KBD_LEDS, 0xff);
    let snap = w.finish();

    let mut hid = UsbCompositeHidInputHandle::new();
    hid.load_state(&snap).unwrap();
    assert_eq!(hid.keyboard_leds(), KEYBOARD_LED_MASK);
}
