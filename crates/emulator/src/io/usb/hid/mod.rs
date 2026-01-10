pub mod keyboard;
pub mod mouse;
pub mod usage;

const USB_DESCRIPTOR_TYPE_DEVICE: u8 = 0x01;
const USB_DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
const USB_DESCRIPTOR_TYPE_STRING: u8 = 0x03;

const USB_DESCRIPTOR_TYPE_INTERFACE: u8 = 0x04;
const USB_DESCRIPTOR_TYPE_ENDPOINT: u8 = 0x05;

const USB_DESCRIPTOR_TYPE_HID: u8 = 0x21;
const USB_DESCRIPTOR_TYPE_HID_REPORT: u8 = 0x22;

const USB_REQUEST_GET_STATUS: u8 = 0x00;
const USB_REQUEST_CLEAR_FEATURE: u8 = 0x01;
const USB_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
const USB_REQUEST_GET_CONFIGURATION: u8 = 0x08;
const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;
const USB_REQUEST_SET_FEATURE: u8 = 0x03;
const USB_REQUEST_SET_ADDRESS: u8 = 0x05;
const USB_REQUEST_GET_INTERFACE: u8 = 0x0a;
const USB_REQUEST_SET_INTERFACE: u8 = 0x0b;

const USB_FEATURE_ENDPOINT_HALT: u16 = 0x0000;
const USB_FEATURE_DEVICE_REMOTE_WAKEUP: u16 = 0x0001;

const HID_REQUEST_GET_REPORT: u8 = 0x01;
const HID_REQUEST_GET_IDLE: u8 = 0x02;
const HID_REQUEST_GET_PROTOCOL: u8 = 0x03;
const HID_REQUEST_SET_REPORT: u8 = 0x09;
const HID_REQUEST_SET_IDLE: u8 = 0x0a;
const HID_REQUEST_SET_PROTOCOL: u8 = 0x0b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidProtocol {
    Boot = 0,
    Report = 1,
}

impl HidProtocol {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::Boot),
            1 => Some(Self::Report),
            _ => None,
        }
    }
}

fn clamp_response(mut data: Vec<u8>, setup_w_length: u16) -> Vec<u8> {
    let requested = setup_w_length as usize;
    if data.len() > requested {
        data.truncate(requested);
    }
    data
}

fn build_string_descriptor_utf16le(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + s.len() * 2);
    out.push(0); // bLength filled in later
    out.push(USB_DESCRIPTOR_TYPE_STRING);
    for ch in s.encode_utf16() {
        out.extend_from_slice(&ch.to_le_bytes());
    }
    out[0] = out.len() as u8;
    out
}

/// Maps a subset of `KeyboardEvent.code` values to USB HID usage IDs.
///
/// This allows the browser/platform layer to translate keyboard events into HID
/// reports for [`keyboard::UsbHidKeyboard`].
pub fn hid_usage_from_js_code(code: &str) -> Option<u8> {
    Some(match code {
        "KeyA" => 0x04,
        "KeyB" => 0x05,
        "KeyC" => 0x06,
        "KeyD" => 0x07,
        "KeyE" => 0x08,
        "KeyF" => 0x09,
        "KeyG" => 0x0a,
        "KeyH" => 0x0b,
        "KeyI" => 0x0c,
        "KeyJ" => 0x0d,
        "KeyK" => 0x0e,
        "KeyL" => 0x0f,
        "KeyM" => 0x10,
        "KeyN" => 0x11,
        "KeyO" => 0x12,
        "KeyP" => 0x13,
        "KeyQ" => 0x14,
        "KeyR" => 0x15,
        "KeyS" => 0x16,
        "KeyT" => 0x17,
        "KeyU" => 0x18,
        "KeyV" => 0x19,
        "KeyW" => 0x1a,
        "KeyX" => 0x1b,
        "KeyY" => 0x1c,
        "KeyZ" => 0x1d,

        "Digit1" => 0x1e,
        "Digit2" => 0x1f,
        "Digit3" => 0x20,
        "Digit4" => 0x21,
        "Digit5" => 0x22,
        "Digit6" => 0x23,
        "Digit7" => 0x24,
        "Digit8" => 0x25,
        "Digit9" => 0x26,
        "Digit0" => 0x27,

        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2a,
        "Tab" => 0x2b,
        "Space" => 0x2c,

        "Minus" => 0x2d,
        "Equal" => 0x2e,
        "BracketLeft" => 0x2f,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,

        "CapsLock" => 0x39,

        "F1" => 0x3a,
        "F2" => 0x3b,
        "F3" => 0x3c,
        "F4" => 0x3d,
        "F5" => 0x3e,
        "F6" => 0x3f,
        "F7" => 0x40,
        "F8" => 0x41,
        "F9" => 0x42,
        "F10" => 0x43,
        "F11" => 0x44,
        "F12" => 0x45,

        "PrintScreen" => 0x46,
        "ScrollLock" => 0x47,
        "Pause" => 0x48,
        "Insert" => 0x49,
        "Home" => 0x4a,
        "PageUp" => 0x4b,
        "Delete" => 0x4c,
        "End" => 0x4d,
        "PageDown" => 0x4e,
        "ArrowRight" => 0x4f,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,

        _ => return None,
    })
}
