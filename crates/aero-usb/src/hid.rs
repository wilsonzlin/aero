use crate::usb::{SetupPacket, UsbDevice, UsbHandshake, UsbSpeed};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

extern crate alloc;

const REQ_GET_STATUS: u8 = 0x00;
const REQ_SET_ADDRESS: u8 = 0x05;
const REQ_GET_DESCRIPTOR: u8 = 0x06;
const REQ_GET_CONFIGURATION: u8 = 0x08;
const REQ_SET_CONFIGURATION: u8 = 0x09;

const REQ_HID_GET_IDLE: u8 = 0x02;
const REQ_HID_GET_PROTOCOL: u8 = 0x03;
const REQ_HID_SET_REPORT: u8 = 0x09;
const REQ_HID_SET_IDLE: u8 = 0x0A;
const REQ_HID_SET_PROTOCOL: u8 = 0x0B;

const DESC_DEVICE: u8 = 0x01;
const DESC_CONFIGURATION: u8 = 0x02;
const DESC_STRING: u8 = 0x03;
const DESC_HID: u8 = 0x21;
const DESC_REPORT: u8 = 0x22;

const KEYBOARD_REPORT_DESCRIPTOR_LEN: u16 = 45;
const MOUSE_REPORT_DESCRIPTOR_LEN: u16 = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ep0Stage {
    Idle,
    DataIn,
    DataOut,
    StatusIn,
    StatusOut,
}

#[derive(Debug)]
struct Ep0Control {
    stage: Ep0Stage,
    setup: Option<SetupPacket>,
    in_data: Vec<u8>,
    in_offset: usize,
    out_expected: usize,
    out_data: Vec<u8>,
}

impl Ep0Control {
    fn new() -> Self {
        Self {
            stage: Ep0Stage::Idle,
            setup: None,
            in_data: Vec::new(),
            in_offset: 0,
            out_expected: 0,
            out_data: Vec::new(),
        }
    }

    fn begin(&mut self, setup: SetupPacket) {
        self.setup = Some(setup);
        self.in_data.clear();
        self.in_offset = 0;
        self.out_expected = 0;
        self.out_data.clear();

        if setup.length == 0 {
            self.stage = Ep0Stage::StatusIn;
            return;
        }

        if setup.request_type & 0x80 != 0 {
            self.stage = Ep0Stage::DataIn;
        } else {
            self.stage = Ep0Stage::DataOut;
            self.out_expected = setup.length as usize;
        }
    }

    fn setup(&self) -> SetupPacket {
        self.setup.expect("control transfer missing SETUP")
    }
}

fn string_descriptor_utf16le(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + s.len() * 2);
    out.push(0); // bLength placeholder
    out.push(DESC_STRING);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out[0] = out.len() as u8;
    out
}

fn string_descriptor_langid(langid: u16) -> [u8; 4] {
    let [l0, l1] = langid.to_le_bytes();
    [4, DESC_STRING, l0, l1]
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct KeyboardReport {
    pub modifiers: u8,
    pub reserved: u8,
    pub keys: [u8; 6],
}

impl KeyboardReport {
    pub fn empty() -> Self {
        Self {
            modifiers: 0,
            reserved: 0,
            keys: [0; 6],
        }
    }
}

pub struct UsbHidKeyboard {
    address: u8,
    pending_address: Option<u8>,
    configuration: u8,
    pending_configuration: Option<u8>,
    protocol: u8,
    idle_rate: u8,
    ep0: Ep0Control,

    report: KeyboardReport,
    pending_reports: VecDeque<[u8; 8]>,
}

impl UsbHidKeyboard {
    pub fn new() -> Self {
        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            protocol: 1, // Report protocol by default.
            idle_rate: 0,
            ep0: Ep0Control::new(),
            report: KeyboardReport::empty(),
            pending_reports: VecDeque::new(),
        }
    }

    pub fn key_event(&mut self, usage: u8, pressed: bool) {
        if (0xE0..=0xE7).contains(&usage) {
            let bit = 1u8 << (usage - 0xE0);
            if pressed {
                self.report.modifiers |= bit;
            } else {
                self.report.modifiers &= !bit;
            }
            self.enqueue_report();
            return;
        }

        if pressed {
            if self.report.keys.iter().any(|&k| k == usage) {
                return;
            }
            if let Some(slot) = self.report.keys.iter_mut().find(|k| **k == 0) {
                *slot = usage;
            }
        } else {
            for key in &mut self.report.keys {
                if *key == usage {
                    *key = 0;
                }
            }
            let mut compacted = [0u8; 6];
            let mut idx = 0;
            for &k in &self.report.keys {
                if k != 0 && idx < compacted.len() {
                    compacted[idx] = k;
                    idx += 1;
                }
            }
            self.report.keys = compacted;
        }

        self.enqueue_report();
    }

    fn enqueue_report(&mut self) {
        let mut bytes = [0u8; 8];
        bytes[0] = self.report.modifiers;
        bytes[1] = self.report.reserved;
        bytes[2..].copy_from_slice(&self.report.keys);
        self.pending_reports.push_back(bytes);
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
        }
    }

    fn device_descriptor() -> &'static [u8] {
        // Full-speed USB HID keyboard; bMaxPacketSize0 = 8 (common for HID).
        static DESC: [u8; 18] = [
            18,
            DESC_DEVICE,
            0x10,
            0x01,
            0x00,
            0x00,
            0x00,
            8,
            0x34,
            0x12,
            0x01,
            0x00,
            0x00,
            0x01,
            1,
            2,
            0,
            1,
        ];
        &DESC
    }

    fn report_descriptor() -> &'static [u8] {
        static DESC: &[u8] = &[
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x06, // Usage (Keyboard)
            0xA1, 0x01, // Collection (Application)
            0x05, 0x07, // Usage Page (Keyboard)
            0x19, 0xE0, // Usage Minimum (Left Control)
            0x29, 0xE7, // Usage Maximum (Right GUI)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x01, // Logical Maximum (1)
            0x75, 0x01, // Report Size (1)
            0x95, 0x08, // Report Count (8)
            0x81, 0x02, // Input (Data, Variable, Absolute)
            0x95, 0x01, // Report Count (1)
            0x75, 0x08, // Report Size (8)
            0x81, 0x01, // Input (Constant)
            0x95, 0x06, // Report Count (6)
            0x75, 0x08, // Report Size (8)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x65, // Logical Maximum (101)
            0x05, 0x07, // Usage Page (Keyboard)
            0x19, 0x00, // Usage Minimum (0)
            0x29, 0x65, // Usage Maximum (101)
            0x81, 0x00, // Input (Data, Array)
            0xC0, // End Collection
        ];
        DESC
    }

    fn configuration_descriptor() -> &'static [u8] {
        static DESC: [u8; 34] = {
            let [rl0, rl1] = KEYBOARD_REPORT_DESCRIPTOR_LEN.to_le_bytes();
            let [tl0, tl1] = (34u16).to_le_bytes();
            [
                // Configuration descriptor.
                9,
                DESC_CONFIGURATION,
                tl0,
                tl1,
                1,
                1,
                0,
                0xA0,
                50,
                // Interface descriptor.
                9,
                0x04,
                0,
                0,
                1,
                0x03,
                0x01,
                0x01,
                0,
                // HID descriptor.
                9,
                DESC_HID,
                0x11,
                0x01,
                0,
                1,
                DESC_REPORT,
                rl0,
                rl1,
                // Endpoint descriptor (Interrupt IN endpoint 1).
                7,
                0x05,
                0x81,
                0x03,
                8,
                0,
                10,
            ]
        };
        &DESC
    }

    fn hid_descriptor_from_config() -> &'static [u8] {
        // HID descriptor begins after config (9) + interface (9).
        &Self::configuration_descriptor()[18..27]
    }

    fn get_descriptor(&self, desc_type: u8, index: u8) -> Option<Vec<u8>> {
        match desc_type {
            DESC_DEVICE => Some(Self::device_descriptor().to_vec()),
            DESC_CONFIGURATION => Some(Self::configuration_descriptor().to_vec()),
            DESC_STRING => match index {
                0 => Some(string_descriptor_langid(0x0409).to_vec()), // en-US
                1 => Some(string_descriptor_utf16le("Aero")),
                2 => Some(string_descriptor_utf16le("Aero HID Keyboard")),
                _ => Some(vec![0, DESC_STRING]),
            },
            DESC_HID => Some(Self::hid_descriptor_from_config().to_vec()),
            DESC_REPORT => Some(Self::report_descriptor().to_vec()),
            _ => None,
        }
    }

    fn handle_setup_inner(&mut self, setup: SetupPacket) -> Option<Vec<u8>> {
        match (setup.request_type, setup.request) {
            (0x80, REQ_GET_DESCRIPTOR) | (0x81, REQ_GET_DESCRIPTOR) => {
                let desc_type = (setup.value >> 8) as u8;
                let index = (setup.value & 0xFF) as u8;
                self.get_descriptor(desc_type, index)
            }
            (0x80, REQ_GET_CONFIGURATION) => Some(vec![self.configuration]),
            (0x80, REQ_GET_STATUS) | (0x81, REQ_GET_STATUS) => Some(vec![0, 0]),
            (0xA1, REQ_HID_GET_PROTOCOL) => Some(vec![self.protocol]),
            (0xA1, REQ_HID_GET_IDLE) => Some(vec![self.idle_rate]),
            _ => None,
        }
    }

    fn handle_no_data_request(&mut self, setup: SetupPacket) {
        match (setup.request_type, setup.request) {
            (0x00, REQ_SET_ADDRESS) => self.pending_address = Some((setup.value & 0x7F) as u8),
            (0x00, REQ_SET_CONFIGURATION) => {
                self.pending_configuration = Some((setup.value & 0xFF) as u8)
            }
            (0x21, REQ_HID_SET_IDLE) => self.idle_rate = (setup.value >> 8) as u8,
            (0x21, REQ_HID_SET_PROTOCOL) => self.protocol = (setup.value & 0xFF) as u8,
            _ => {}
        }
    }
}

impl Default for UsbHidKeyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDevice for UsbHidKeyboard {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    fn reset(&mut self) {
        self.address = 0;
        self.pending_address = None;
        self.configuration = 0;
        self.pending_configuration = None;
        self.protocol = 1;
        self.idle_rate = 0;
        self.ep0 = Ep0Control::new();
        self.report = KeyboardReport::empty();
        self.pending_reports.clear();
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: SetupPacket) {
        self.ep0.begin(setup);

        if setup.length == 0 {
            self.handle_no_data_request(setup);
            return;
        }

        if setup.request_type & 0x80 != 0 {
            let mut data = self.handle_setup_inner(setup).unwrap_or_default();
            data.truncate(setup.length as usize);
            self.ep0.in_data = data;
            return;
        }
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataOut => {
                self.ep0.out_data.extend_from_slice(data);
                if self.ep0.out_data.len() >= self.ep0.out_expected {
                    let setup = self.ep0.setup();
                    match (setup.request_type, setup.request) {
                        (0x21, REQ_HID_SET_REPORT) => {
                            // Ignore LED/output reports; keep the transfer successful.
                        }
                        _ => {}
                    }
                    self.handle_no_data_request(setup);
                    self.ep0.stage = Ep0Stage::StatusIn;
                }
                UsbHandshake::Ack { bytes: data.len() }
            }
            Ep0Stage::StatusOut => {
                self.ep0.stage = Ep0Stage::Idle;
                self.ep0.setup = None;
                self.finalize_control();
                UsbHandshake::Ack { bytes: 0 }
            }
            _ => UsbHandshake::Nak,
        }
    }

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> UsbHandshake {
        if ep == 1 {
            let Some(report) = self.pending_reports.pop_front() else {
                return UsbHandshake::Nak;
            };
            let len = buf.len().min(report.len());
            buf[..len].copy_from_slice(&report[..len]);
            return UsbHandshake::Ack { bytes: len };
        }

        if ep != 0 {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataIn => {
                let remaining = self.ep0.in_data.len().saturating_sub(self.ep0.in_offset);
                let len = buf.len().min(remaining);
                buf[..len].copy_from_slice(
                    &self.ep0.in_data[self.ep0.in_offset..self.ep0.in_offset + len],
                );
                self.ep0.in_offset += len;
                if self.ep0.in_offset >= self.ep0.in_data.len() {
                    self.ep0.stage = Ep0Stage::StatusOut;
                }
                UsbHandshake::Ack { bytes: len }
            }
            Ep0Stage::StatusIn => {
                self.ep0.stage = Ep0Stage::Idle;
                self.ep0.setup = None;
                self.finalize_control();
                UsbHandshake::Ack { bytes: 0 }
            }
            _ => UsbHandshake::Nak,
        }
    }
}

pub struct UsbHidMouse {
    address: u8,
    pending_address: Option<u8>,
    configuration: u8,
    pending_configuration: Option<u8>,
    protocol: u8,
    idle_rate: u8,
    ep0: Ep0Control,

    buttons: u8,
    pending_reports: VecDeque<[u8; 3]>,
}

impl UsbHidMouse {
    pub fn new() -> Self {
        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            protocol: 1,
            idle_rate: 0,
            ep0: Ep0Control::new(),
            buttons: 0,
            pending_reports: VecDeque::new(),
        }
    }

    pub fn movement(&mut self, dx: i32, dy: i32) {
        let dx = dx.clamp(-127, 127) as i8 as u8;
        let dy = dy.clamp(-127, 127) as i8 as u8;
        self.pending_reports
            .push_back([self.buttons & 0x07, dx, dy]);
    }

    pub fn button_event(&mut self, button_mask: u8, pressed: bool) {
        if pressed {
            self.buttons |= button_mask;
        } else {
            self.buttons &= !button_mask;
        }
        self.pending_reports.push_back([self.buttons & 0x07, 0, 0]);
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
        }
    }

    fn device_descriptor() -> &'static [u8] {
        static DESC: [u8; 18] = [
            18,
            DESC_DEVICE,
            0x10,
            0x01,
            0x00,
            0x00,
            0x00,
            8,
            0x34,
            0x12,
            0x02,
            0x00,
            0x00,
            0x01,
            1,
            3,
            0,
            1,
        ];
        &DESC
    }

    fn report_descriptor() -> &'static [u8] {
        static DESC: &[u8] = &[
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x02, // Usage (Mouse)
            0xA1, 0x01, // Collection (Application)
            0x09, 0x01, //   Usage (Pointer)
            0xA1, 0x00, //   Collection (Physical)
            0x05, 0x09, //     Usage Page (Button)
            0x19, 0x01, //     Usage Minimum (Button 1)
            0x29, 0x03, //     Usage Maximum (Button 3)
            0x15, 0x00, //     Logical Minimum (0)
            0x25, 0x01, //     Logical Maximum (1)
            0x95, 0x03, //     Report Count (3)
            0x75, 0x01, //     Report Size (1)
            0x81, 0x02, //     Input (Data, Variable, Absolute)
            0x95, 0x01, //     Report Count (1)
            0x75, 0x05, //     Report Size (5)
            0x81, 0x01, //     Input (Constant)
            0x05, 0x01, //     Usage Page (Generic Desktop)
            0x09, 0x30, //     Usage (X)
            0x09, 0x31, //     Usage (Y)
            0x15, 0x81, //     Logical Minimum (-127)
            0x25, 0x7F, //     Logical Maximum (127)
            0x75, 0x08, //     Report Size (8)
            0x95, 0x02, //     Report Count (2)
            0x81, 0x06, //     Input (Data, Variable, Relative)
            0xC0, //   End Collection
            0xC0, // End Collection
        ];
        DESC
    }

    fn configuration_descriptor() -> &'static [u8] {
        static DESC: [u8; 34] = {
            let [rl0, rl1] = MOUSE_REPORT_DESCRIPTOR_LEN.to_le_bytes();
            let [tl0, tl1] = (34u16).to_le_bytes();
            [
                9,
                DESC_CONFIGURATION,
                tl0,
                tl1,
                1,
                1,
                0,
                0xA0,
                50,
                9,
                0x04,
                0,
                0,
                1,
                0x03,
                0x01,
                0x02,
                0,
                9,
                DESC_HID,
                0x11,
                0x01,
                0,
                1,
                DESC_REPORT,
                rl0,
                rl1,
                7,
                0x05,
                0x81,
                0x03,
                3,
                0,
                10,
            ]
        };
        &DESC
    }

    fn hid_descriptor_from_config() -> &'static [u8] {
        &Self::configuration_descriptor()[18..27]
    }

    fn get_descriptor(&self, desc_type: u8, index: u8) -> Option<Vec<u8>> {
        match desc_type {
            DESC_DEVICE => Some(Self::device_descriptor().to_vec()),
            DESC_CONFIGURATION => Some(Self::configuration_descriptor().to_vec()),
            DESC_STRING => match index {
                0 => Some(string_descriptor_langid(0x0409).to_vec()),
                1 => Some(string_descriptor_utf16le("Aero")),
                3 => Some(string_descriptor_utf16le("Aero HID Mouse")),
                _ => Some(vec![0, DESC_STRING]),
            },
            DESC_HID => Some(Self::hid_descriptor_from_config().to_vec()),
            DESC_REPORT => Some(Self::report_descriptor().to_vec()),
            _ => None,
        }
    }

    fn handle_setup_inner(&mut self, setup: SetupPacket) -> Option<Vec<u8>> {
        match (setup.request_type, setup.request) {
            (0x80, REQ_GET_DESCRIPTOR) | (0x81, REQ_GET_DESCRIPTOR) => {
                let desc_type = (setup.value >> 8) as u8;
                let index = (setup.value & 0xFF) as u8;
                self.get_descriptor(desc_type, index)
            }
            (0x80, REQ_GET_CONFIGURATION) => Some(vec![self.configuration]),
            (0x80, REQ_GET_STATUS) | (0x81, REQ_GET_STATUS) => Some(vec![0, 0]),
            (0xA1, REQ_HID_GET_PROTOCOL) => Some(vec![self.protocol]),
            (0xA1, REQ_HID_GET_IDLE) => Some(vec![self.idle_rate]),
            _ => None,
        }
    }

    fn handle_no_data_request(&mut self, setup: SetupPacket) {
        match (setup.request_type, setup.request) {
            (0x00, REQ_SET_ADDRESS) => self.pending_address = Some((setup.value & 0x7F) as u8),
            (0x00, REQ_SET_CONFIGURATION) => {
                self.pending_configuration = Some((setup.value & 0xFF) as u8)
            }
            (0x21, REQ_HID_SET_IDLE) => self.idle_rate = (setup.value >> 8) as u8,
            (0x21, REQ_HID_SET_PROTOCOL) => self.protocol = (setup.value & 0xFF) as u8,
            _ => {}
        }
    }
}

impl Default for UsbHidMouse {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDevice for UsbHidMouse {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    fn reset(&mut self) {
        self.address = 0;
        self.pending_address = None;
        self.configuration = 0;
        self.pending_configuration = None;
        self.protocol = 1;
        self.idle_rate = 0;
        self.ep0 = Ep0Control::new();
        self.buttons = 0;
        self.pending_reports.clear();
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: SetupPacket) {
        self.ep0.begin(setup);

        if setup.length == 0 {
            self.handle_no_data_request(setup);
            return;
        }

        if setup.request_type & 0x80 != 0 {
            let mut data = self.handle_setup_inner(setup).unwrap_or_default();
            data.truncate(setup.length as usize);
            self.ep0.in_data = data;
        }
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataOut => {
                self.ep0.out_data.extend_from_slice(data);
                if self.ep0.out_data.len() >= self.ep0.out_expected {
                    let setup = self.ep0.setup();
                    self.handle_no_data_request(setup);
                    self.ep0.stage = Ep0Stage::StatusIn;
                }
                UsbHandshake::Ack { bytes: data.len() }
            }
            Ep0Stage::StatusOut => {
                self.ep0.stage = Ep0Stage::Idle;
                self.ep0.setup = None;
                self.finalize_control();
                UsbHandshake::Ack { bytes: 0 }
            }
            _ => UsbHandshake::Nak,
        }
    }

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> UsbHandshake {
        if ep == 1 {
            let Some(report) = self.pending_reports.pop_front() else {
                return UsbHandshake::Nak;
            };
            let len = buf.len().min(report.len());
            buf[..len].copy_from_slice(&report[..len]);
            return UsbHandshake::Ack { bytes: len };
        }

        if ep != 0 {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataIn => {
                let remaining = self.ep0.in_data.len().saturating_sub(self.ep0.in_offset);
                let len = buf.len().min(remaining);
                buf[..len].copy_from_slice(
                    &self.ep0.in_data[self.ep0.in_offset..self.ep0.in_offset + len],
                );
                self.ep0.in_offset += len;
                if self.ep0.in_offset >= self.ep0.in_data.len() {
                    self.ep0.stage = Ep0Stage::StatusOut;
                }
                UsbHandshake::Ack { bytes: len }
            }
            Ep0Stage::StatusIn => {
                self.ep0.stage = Ep0Stage::Idle;
                self.ep0.setup = None;
                self.finalize_control();
                UsbHandshake::Ack { bytes: 0 }
            }
            _ => UsbHandshake::Nak,
        }
    }
}
