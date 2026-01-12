pub mod composite;
pub mod gamepad;
pub mod keyboard;
pub mod mouse;
pub mod passthrough;
pub mod report_descriptor;
pub mod usage;
pub mod webhid;

pub use composite::UsbCompositeHidInputHandle;
pub use gamepad::UsbHidGamepadHandle;
pub use keyboard::UsbHidKeyboardHandle;
pub use mouse::UsbHidMouseHandle;

pub use report_descriptor::{
    has_report_ids, max_feature_report_bytes, max_input_report_bytes, max_output_report_bytes,
    parse_report_descriptor, report_bits, report_bytes, synthesize_report_descriptor,
    validate_collections, HidCollectionInfo, HidDescriptorError, HidReportDescriptorParseResult,
    HidReportInfo, HidReportItem, ValidationSummary,
};

pub use passthrough::{UsbHidPassthrough, UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};

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
    // USB string descriptors encode `bLength` as a u8, and strings are UTF-16LE. This caps the
    // total descriptor size to 254 bytes (2-byte header + up to 126 UTF-16 code units) and avoids
    // truncating surrogate pairs mid-character.
    const MAX_LEN: usize = 254;

    let mut out = Vec::with_capacity(MAX_LEN);
    out.push(0); // bLength placeholder
    out.push(USB_DESCRIPTOR_TYPE_STRING);
    for ch in s.chars() {
        let mut buf = [0u16; 2];
        let units = ch.encode_utf16(&mut buf);
        let needed = units.len() * 2;
        if out.len() + needed > MAX_LEN {
            break;
        }
        for unit in units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out[0] = out.len() as u8;
    out
}

/// Maps a subset of `KeyboardEvent.code` values to USB HID usage IDs.
///
/// This allows the browser/platform layer to translate keyboard events into HID
/// reports for [`keyboard::UsbHidKeyboard`].
pub fn hid_usage_from_js_code(code: &str) -> Option<u8> {
    usage::keyboard_code_to_usage(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_descriptors_are_capped_to_u8_length_and_remain_valid_utf16() {
        let long = "😀".repeat(1000);
        let desc = build_string_descriptor_utf16le(&long);
        assert_eq!(desc.len(), 254);
        assert_eq!(desc[0] as usize, desc.len());
        assert_eq!(desc[1], USB_DESCRIPTOR_TYPE_STRING);

        let payload = &desc[2..];
        assert_eq!(payload.len() % 2, 0);
        let units: Vec<u16> = payload
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        String::from_utf16(&units).expect("payload must be valid UTF-16");
    }
}
