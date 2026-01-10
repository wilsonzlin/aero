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
