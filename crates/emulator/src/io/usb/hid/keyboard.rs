use std::collections::VecDeque;

use crate::io::usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

use super::{
    build_string_descriptor_utf16le, clamp_response, HidProtocol, HID_REQUEST_GET_IDLE,
    HID_REQUEST_GET_PROTOCOL, HID_REQUEST_GET_REPORT, HID_REQUEST_SET_IDLE, HID_REQUEST_SET_PROTOCOL,
    HID_REQUEST_SET_REPORT, USB_DESCRIPTOR_TYPE_CONFIGURATION, USB_DESCRIPTOR_TYPE_DEVICE,
    USB_DESCRIPTOR_TYPE_HID, USB_DESCRIPTOR_TYPE_HID_REPORT, USB_DESCRIPTOR_TYPE_STRING,
    USB_FEATURE_DEVICE_REMOTE_WAKEUP, USB_FEATURE_ENDPOINT_HALT, USB_REQUEST_CLEAR_FEATURE,
    USB_REQUEST_GET_CONFIGURATION, USB_REQUEST_GET_DESCRIPTOR, USB_REQUEST_GET_INTERFACE,
    USB_REQUEST_GET_STATUS, USB_REQUEST_SET_ADDRESS, USB_REQUEST_SET_CONFIGURATION,
    USB_REQUEST_SET_FEATURE, USB_REQUEST_SET_INTERFACE,
};

const INTERRUPT_IN_EP: u8 = 0x81;
const MAX_PENDING_REPORTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardReport {
    pub modifiers: u8,
    pub reserved: u8,
    pub keys: [u8; 6],
}

impl KeyboardReport {
    pub fn to_bytes(self) -> [u8; 8] {
        [
            self.modifiers,
            self.reserved,
            self.keys[0],
            self.keys[1],
            self.keys[2],
            self.keys[3],
            self.keys[4],
            self.keys[5],
        ]
    }
}

pub struct UsbHidKeyboard {
    address: u8,
    configuration: u8,
    remote_wakeup_enabled: bool,
    interrupt_in_halted: bool,
    idle_rate: u8,
    protocol: HidProtocol,
    leds: u8,

    modifiers: u8,
    pressed_keys: Vec<u8>,

    last_report: [u8; 8],
    pending_reports: VecDeque<[u8; 8]>,
}

impl Default for UsbHidKeyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbHidKeyboard {
    pub fn new() -> Self {
        Self {
            address: 0,
            configuration: 0,
            remote_wakeup_enabled: false,
            interrupt_in_halted: false,
            idle_rate: 0,
            protocol: HidProtocol::Report,
            leds: 0,
            modifiers: 0,
            pressed_keys: Vec::new(),
            last_report: [0; 8],
            pending_reports: VecDeque::new(),
        }
    }

    pub fn key_event(&mut self, usage: u8, pressed: bool) {
        if usage == 0 {
            return;
        }

        let mut changed = false;
        if let Some(bit) = modifier_bit(usage) {
            let before = self.modifiers;
            if pressed {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
            changed = before != self.modifiers;
        } else if pressed {
            if !self.pressed_keys.contains(&usage) {
                self.pressed_keys.push(usage);
                changed = true;
            }
        } else {
            let before_len = self.pressed_keys.len();
            self.pressed_keys.retain(|&k| k != usage);
            changed = before_len != self.pressed_keys.len();
        }

        if changed {
            self.enqueue_current_report();
        }
    }

    pub fn current_input_report(&self) -> KeyboardReport {
        let mut keys = [0u8; 6];
        if self.pressed_keys.len() > 6 {
            keys.fill(0x01); // ErrorRollOver
        } else {
            for (idx, &usage) in self.pressed_keys.iter().take(6).enumerate() {
                keys[idx] = usage;
            }
        }
        KeyboardReport {
            modifiers: self.modifiers,
            reserved: 0,
            keys,
        }
    }

    fn enqueue_current_report(&mut self) {
        let report = self.current_input_report().to_bytes();
        if report != self.last_report {
            self.last_report = report;
            if self.pending_reports.len() >= MAX_PENDING_REPORTS {
                self.pending_reports.pop_front();
            }
            self.pending_reports.push_back(report);
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(build_string_descriptor_utf16le("Aero")),
            2 => Some(build_string_descriptor_utf16le("Aero USB HID Keyboard")),
            _ => None,
        }
    }

    fn hid_descriptor_bytes(&self) -> [u8; 9] {
        let report_len = HID_REPORT_DESCRIPTOR.len() as u16;
        [
            0x09,                  // bLength
            USB_DESCRIPTOR_TYPE_HID, // bDescriptorType
            0x11,
            0x01, // bcdHID (1.11)
            0x00, // bCountryCode
            0x01, // bNumDescriptors
            USB_DESCRIPTOR_TYPE_HID_REPORT, // bDescriptorType (Report)
            (report_len & 0x00ff) as u8,
            (report_len >> 8) as u8,
        ]
    }
}

impl UsbDeviceModel for UsbHidKeyboard {
    fn get_device_descriptor(&self) -> &'static [u8] {
        &DEVICE_DESCRIPTOR
    }

    fn get_config_descriptor(&self) -> &'static [u8] {
        &CONFIG_DESCRIPTOR
    }

    fn get_hid_report_descriptor(&self) -> &'static [u8] {
        &HID_REPORT_DESCRIPTOR
    }

    fn handle_control_request(&mut self, setup: SetupPacket, data_stage: Option<&[u8]>) -> ControlResponse {
        match (setup.request_type(), setup.recipient()) {
            (RequestType::Standard, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    let mut status: u16 = 0;
                    if self.remote_wakeup_enabled {
                        status |= 1 << 1;
                    }
                    ControlResponse::Data(clamp_response(status.to_le_bytes().to_vec(), setup.w_length))
                }
                USB_REQUEST_CLEAR_FEATURE => match setup.w_value {
                    USB_FEATURE_DEVICE_REMOTE_WAKEUP => {
                        if setup.request_direction() != RequestDirection::HostToDevice || setup.w_index != 0 {
                            return ControlResponse::Stall;
                        }
                        self.remote_wakeup_enabled = false;
                        ControlResponse::Ack
                    }
                    _ => ControlResponse::Stall,
                },
                USB_REQUEST_SET_FEATURE => match setup.w_value {
                    USB_FEATURE_DEVICE_REMOTE_WAKEUP => {
                        if setup.request_direction() != RequestDirection::HostToDevice || setup.w_index != 0 {
                            return ControlResponse::Stall;
                        }
                        self.remote_wakeup_enabled = true;
                        ControlResponse::Ack
                    }
                    _ => ControlResponse::Stall,
                },
                USB_REQUEST_SET_ADDRESS => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value > 127 {
                        return ControlResponse::Stall;
                    }
                    self.address = (setup.w_value & 0x00ff) as u8;
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost {
                        return ControlResponse::Stall;
                    }
                    let desc_type = setup.descriptor_type();
                    let desc_index = setup.descriptor_index();
                    let data = match desc_type {
                        USB_DESCRIPTOR_TYPE_DEVICE => Some(self.get_device_descriptor().to_vec()),
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => Some(self.get_config_descriptor().to_vec()),
                        USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                        _ => None,
                    };
                    data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                        .unwrap_or(ControlResponse::Stall)
                }
                USB_REQUEST_SET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    let config = (setup.w_value & 0x00ff) as u8;
                    if config > 1 {
                        return ControlResponse::Stall;
                    }
                    self.configuration = config;
                    if self.configuration == 0 {
                        self.pending_reports.clear();
                    }
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.configuration], setup.w_length))
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Interface) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0, 0], setup.w_length))
                }
                USB_REQUEST_GET_INTERFACE => {
                    if setup.request_direction() != RequestDirection::DeviceToHost {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index == 0 {
                        ControlResponse::Data(clamp_response(vec![0], setup.w_length))
                    } else {
                        ControlResponse::Stall
                    }
                }
                USB_REQUEST_SET_INTERFACE => {
                    if setup.request_direction() != RequestDirection::HostToDevice {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index == 0 && setup.w_value == 0 {
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    let desc_type = setup.descriptor_type();
                    let data = match desc_type {
                        USB_DESCRIPTOR_TYPE_HID_REPORT => Some(self.get_hid_report_descriptor().to_vec()),
                        USB_DESCRIPTOR_TYPE_HID => Some(self.hid_descriptor_bytes().to_vec()),
                        _ => None,
                    };
                    data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                        .unwrap_or(ControlResponse::Stall)
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Endpoint) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_value != 0 {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index != INTERRUPT_IN_EP as u16 {
                        return ControlResponse::Stall;
                    }
                    let status: u16 = if self.interrupt_in_halted { 1 } else { 0 };
                    ControlResponse::Data(clamp_response(status.to_le_bytes().to_vec(), setup.w_length))
                }
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_length != 0 {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT && setup.w_index == INTERRUPT_IN_EP as u16 {
                        self.interrupt_in_halted = false;
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_length != 0 {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT && setup.w_index == INTERRUPT_IN_EP as u16 {
                        self.interrupt_in_halted = true;
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Interface) => match setup.b_request {
                HID_REQUEST_GET_REPORT => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    // wValue high byte: Report Type (1=input, 2=output, 3=feature)
                    let report_type = (setup.w_value >> 8) as u8;
                    match report_type {
                        1 => ControlResponse::Data(clamp_response(
                            self.current_input_report().to_bytes().to_vec(),
                            setup.w_length,
                        )),
                        2 => ControlResponse::Data(clamp_response(vec![self.leds], setup.w_length)),
                        _ => ControlResponse::Stall,
                    }
                }
                HID_REQUEST_SET_REPORT => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    let report_type = (setup.w_value >> 8) as u8;
                    match (report_type, data_stage) {
                        (2, Some(data)) if !data.is_empty() => {
                            self.leds = data[0];
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
                    }
                }
                HID_REQUEST_GET_IDLE => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.idle_rate], setup.w_length))
                }
                HID_REQUEST_SET_IDLE => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    self.idle_rate = (setup.w_value >> 8) as u8;
                    ControlResponse::Ack
                }
                HID_REQUEST_GET_PROTOCOL => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.protocol as u8], setup.w_length))
                }
                HID_REQUEST_SET_PROTOCOL => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    if let Some(proto) = HidProtocol::from_u16(setup.w_value) {
                        self.protocol = proto;
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                _ => ControlResponse::Stall,
            },
            _ => ControlResponse::Stall,
        }
    }

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        if ep != INTERRUPT_IN_EP {
            return None;
        }
        if self.configuration == 0 || self.interrupt_in_halted {
            return None;
        }
        self.pending_reports.pop_front().map(|r| r.to_vec())
    }
}

fn modifier_bit(usage: u8) -> Option<u8> {
    (0xe0..=0xe7)
        .contains(&usage)
        .then(|| 1u8 << (usage - 0xe0))
}

// USB device descriptor (Keyboard)
static DEVICE_DESCRIPTOR: [u8; 18] = [
    0x12, // bLength
    USB_DESCRIPTOR_TYPE_DEVICE,
    0x00,
    0x02, // bcdUSB (2.00)
    0x00, // bDeviceClass (per interface)
    0x00, // bDeviceSubClass
    0x00, // bDeviceProtocol
    0x40, // bMaxPacketSize0 (64)
    0x34,
    0x12, // idVendor (0x1234)
    0x01,
    0x00, // idProduct (0x0001)
    0x00,
    0x01, // bcdDevice (1.00)
    0x01, // iManufacturer
    0x02, // iProduct
    0x00, // iSerialNumber
    0x01, // bNumConfigurations
];

// USB configuration descriptor tree:
//   Config(9) + Interface(9) + HID(9) + Endpoint(7) = 34 bytes
static CONFIG_DESCRIPTOR: [u8; 34] = [
    // Configuration descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_CONFIGURATION,
    34, 0x00, // wTotalLength
    0x01, // bNumInterfaces
    0x01, // bConfigurationValue
    0x00, // iConfiguration
    0xa0, // bmAttributes (bus powered + remote wake)
    50,  // bMaxPower (100mA)
    // Interface descriptor
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    0x00, // bInterfaceNumber
    0x00, // bAlternateSetting
    0x01, // bNumEndpoints
    0x03, // bInterfaceClass (HID)
    0x01, // bInterfaceSubClass (Boot)
    0x01, // bInterfaceProtocol (Keyboard)
    0x00, // iInterface
    // HID descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_HID,
    0x11,
    0x01, // bcdHID (1.11)
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    USB_DESCRIPTOR_TYPE_HID_REPORT,
    HID_REPORT_DESCRIPTOR.len() as u8,
    0x00, // wDescriptorLength
    // Endpoint descriptor (Interrupt IN)
    0x07, // bLength
    super::USB_DESCRIPTOR_TYPE_ENDPOINT,
    INTERRUPT_IN_EP, // bEndpointAddress
    0x03, // bmAttributes (Interrupt)
    0x08,
    0x00, // wMaxPacketSize (8)
    0x0a, // bInterval (10ms)
];

static HID_REPORT_DESCRIPTOR: [u8; 63] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xa1, 0x01, // Collection (Application)
    0x05, 0x07, // Usage Page (Keyboard/Keypad)
    0x19, 0xe0, // Usage Minimum (Left Control)
    0x29, 0xe7, // Usage Maximum (Right GUI)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x01, // Logical Maximum (1)
    0x75, 0x01, // Report Size (1)
    0x95, 0x08, // Report Count (8)
    0x81, 0x02, // Input (Data,Var,Abs) Modifier byte
    0x95, 0x01, // Report Count (1)
    0x75, 0x08, // Report Size (8)
    0x81, 0x01, // Input (Const,Array,Abs) Reserved byte
    0x95, 0x05, // Report Count (5)
    0x75, 0x01, // Report Size (1)
    0x05, 0x08, // Usage Page (LEDs)
    0x19, 0x01, // Usage Minimum (Num Lock)
    0x29, 0x05, // Usage Maximum (Kana)
    0x91, 0x02, // Output (Data,Var,Abs) LED report
    0x95, 0x01, // Report Count (1)
    0x75, 0x03, // Report Size (3)
    0x91, 0x01, // Output (Const,Array,Abs) LED padding
    0x95, 0x06, // Report Count (6)
    0x75, 0x08, // Report Size (8)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x65, // Logical Maximum (101)
    0x05, 0x07, // Usage Page (Keyboard/Keypad)
    0x19, 0x00, // Usage Minimum (0)
    0x29, 0x65, // Usage Maximum (101)
    0x81, 0x00, // Input (Data,Array,Abs) Key arrays (6 bytes)
    0xc0, // End Collection
];

#[cfg(test)]
mod tests {
    use super::*;

    fn w_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn configure_keyboard(kb: &mut UsbHidKeyboard) {
        assert_eq!(
            kb.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00,
                    b_request: USB_REQUEST_SET_CONFIGURATION,
                    w_value: 1,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
    }

    #[test]
    fn device_descriptor_is_well_formed() {
        let kb = UsbHidKeyboard::new();
        let dev = kb.get_device_descriptor();
        assert_eq!(dev.len(), 18);
        assert_eq!(dev[0] as usize, dev.len());
        assert_eq!(dev[1], USB_DESCRIPTOR_TYPE_DEVICE);
    }

    #[test]
    fn config_descriptor_has_expected_layout() {
        let kb = UsbHidKeyboard::new();
        let cfg = kb.get_config_descriptor();
        assert_eq!(cfg[0], 0x09);
        assert_eq!(cfg[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(w_le(cfg, 2) as usize, cfg.len());
        assert_eq!(cfg.len(), 34);

        // HID descriptor starts at offset 18 (9 config + 9 interface).
        let hid = &cfg[18..27];
        assert_eq!(hid[0], 0x09);
        assert_eq!(hid[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(hid[6], USB_DESCRIPTOR_TYPE_HID_REPORT);
        assert_eq!(w_le(hid, 7) as usize, HID_REPORT_DESCRIPTOR.len());

        let ep = &cfg[27..34];
        assert_eq!(ep[1], super::super::USB_DESCRIPTOR_TYPE_ENDPOINT);
        assert_eq!(ep[2], INTERRUPT_IN_EP);
    }

    #[test]
    fn keyboard_report_generation_and_rollover() {
        let mut kb = UsbHidKeyboard::new();
        configure_keyboard(&mut kb);

        kb.key_event(0x04, true); // 'a'
        let report = kb.poll_interrupt_in(INTERRUPT_IN_EP).unwrap();
        assert_eq!(report, [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);

        kb.key_event(0xe1, true); // LeftShift
        let report = kb.poll_interrupt_in(INTERRUPT_IN_EP).unwrap();
        assert_eq!(report[0], 0x02);
        assert_eq!(report[2], 0x04);

        // Press 6 additional keys to trigger rollover (>6 non-modifiers).
        for usage in 0x05..=0x0a {
            kb.key_event(usage, true);
        }
        let mut rollover = None;
        while let Some(report) = kb.poll_interrupt_in(INTERRUPT_IN_EP) {
            rollover = Some(report);
        }
        let rollover = rollover.unwrap();
        assert_eq!(&rollover[2..], &[0x01; 6]);

        // Release one key; should go back to explicit list.
        kb.key_event(0x0a, false);
        let report = kb.poll_interrupt_in(INTERRUPT_IN_EP).unwrap();
        assert_ne!(&report[2..], &[0x01; 6]);
        assert_eq!(report[0], 0x02);
    }

    #[test]
    fn keyboard_report_compacts_on_release_property() {
        let mut kb = UsbHidKeyboard::new();
        let keys: [u8; 6] = [0x04, 0x05, 0x06, 0x07, 0x08, 0x09];

        // Simple deterministic PRNG (LCG) to avoid external dependencies.
        let mut seed = 0x1234_5678u32;
        let mut expected: Vec<u8> = Vec::new();

        for _ in 0..10_000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let idx = (seed as usize) % keys.len();
            let usage = keys[idx];
            let pressed = (seed & 0x8000_0000) != 0;

            kb.key_event(usage, pressed);

            // Maintain the expected insertion-ordered, compacted set.
            if pressed {
                if !expected.contains(&usage) {
                    expected.push(usage);
                }
            } else {
                expected.retain(|&k| k != usage);
            }

            let report = kb.current_input_report().to_bytes();
            let report_keys = &report[2..];

            // Verify: no gaps (all zeros are at the end) and ordering matches expectation.
            let first_zero = report_keys.iter().position(|&k| k == 0).unwrap_or(6);
            // If there are zeros, everything after the first zero must be zero.
            for &k in &report_keys[first_zero..] {
                assert_eq!(k, 0);
            }
            // The non-zero prefix matches the expected pressed key list.
            let non_zero_len = first_zero;
            assert_eq!(&report_keys[..non_zero_len], &expected[..non_zero_len]);
        }
    }

    #[test]
    fn keyboard_standard_requests_track_status_bits() {
        let mut kb = UsbHidKeyboard::new();

        // Default: remote wakeup disabled.
        let resp = kb.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: USB_REQUEST_GET_STATUS,
                w_value: 0,
                w_index: 0,
                w_length: 2,
            },
            None,
        );
        assert_eq!(resp, ControlResponse::Data(vec![0x00, 0x00]));

        // Enable remote wakeup.
        assert_eq!(
            kb.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00,
                    b_request: USB_REQUEST_SET_FEATURE,
                    w_value: USB_FEATURE_DEVICE_REMOTE_WAKEUP,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );

        let resp = kb.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: USB_REQUEST_GET_STATUS,
                w_value: 0,
                w_index: 0,
                w_length: 2,
            },
            None,
        );
        assert_eq!(resp, ControlResponse::Data(vec![0x02, 0x00]));

        // Halt endpoint and verify status.
        assert_eq!(
            kb.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x02,
                    b_request: USB_REQUEST_SET_FEATURE,
                    w_value: USB_FEATURE_ENDPOINT_HALT,
                    w_index: INTERRUPT_IN_EP as u16,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );

        let resp = kb.handle_control_request(
            SetupPacket {
                bm_request_type: 0x82,
                b_request: USB_REQUEST_GET_STATUS,
                w_value: 0,
                w_index: INTERRUPT_IN_EP as u16,
                w_length: 2,
            },
            None,
        );
        assert_eq!(resp, ControlResponse::Data(vec![0x01, 0x00]));

        // SET_ADDRESS should be accepted and stored.
        assert_eq!(
            kb.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00,
                    b_request: USB_REQUEST_SET_ADDRESS,
                    w_value: 7,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
        assert_eq!(kb.address, 7);
    }

    #[test]
    fn stalls_on_wrong_direction() {
        let mut kb = UsbHidKeyboard::new();
        let resp = kb.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00, // HostToDevice but GET_DESCRIPTOR expects DeviceToHost.
                b_request: USB_REQUEST_GET_DESCRIPTOR,
                w_value: (USB_DESCRIPTOR_TYPE_DEVICE as u16) << 8,
                w_index: 0,
                w_length: 18,
            },
            None,
        );
        assert_eq!(resp, ControlResponse::Stall);
    }
}
