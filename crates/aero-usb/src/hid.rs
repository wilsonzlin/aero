use crate::usb::{SetupPacket, UsbDevice, UsbHandshake, UsbSpeed};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

extern crate alloc;

const REQ_GET_STATUS: u8 = 0x00;
const REQ_CLEAR_FEATURE: u8 = 0x01;
const REQ_SET_FEATURE: u8 = 0x03;
const REQ_SET_ADDRESS: u8 = 0x05;
const REQ_GET_DESCRIPTOR: u8 = 0x06;
const REQ_GET_CONFIGURATION: u8 = 0x08;
const REQ_SET_CONFIGURATION: u8 = 0x09;
const REQ_GET_INTERFACE: u8 = 0x0A;
const REQ_SET_INTERFACE: u8 = 0x0B;

const FEATURE_ENDPOINT_HALT: u16 = 0x0000;
const FEATURE_DEVICE_REMOTE_WAKEUP: u16 = 0x0001;

const REQ_HID_GET_REPORT: u8 = 0x01;
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

const MAX_PENDING_REPORTS_KEYBOARD: usize = 64;
const MAX_PENDING_REPORTS_MOUSE: usize = 128;
const MAX_PENDING_REPORTS_GAMEPAD: usize = 128;

const KEYBOARD_REPORT_DESCRIPTOR_LEN: u16 = 63;
const MOUSE_REPORT_DESCRIPTOR_LEN: u16 = 52;
const GAMEPAD_REPORT_DESCRIPTOR_LEN: u16 = 76;

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
    stalled: bool,
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
            stalled: false,
        }
    }

    fn begin(&mut self, setup: SetupPacket) {
        self.setup = Some(setup);
        self.in_data.clear();
        self.in_offset = 0;
        self.out_expected = 0;
        self.out_data.clear();
        self.stalled = false;

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

    pub fn to_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0] = self.modifiers;
        out[1] = self.reserved;
        out[2..].copy_from_slice(&self.keys);
        out
    }
}

fn keyboard_modifier_bit(usage: u8) -> Option<u8> {
    if (0xE0..=0xE7).contains(&usage) {
        Some(1u8 << (usage - 0xE0))
    } else {
        None
    }
}

fn build_keyboard_report(modifiers: u8, pressed_keys: &[u8]) -> KeyboardReport {
    let mut keys = [0u8; 6];
    if pressed_keys.len() > 6 {
        keys.fill(0x01); // ErrorRollOver
    } else {
        for (idx, &usage) in pressed_keys.iter().take(6).enumerate() {
            keys[idx] = usage;
        }
    }
    KeyboardReport {
        modifiers,
        reserved: 0,
        keys,
    }
}

pub struct UsbHidKeyboard {
    address: u8,
    pending_address: Option<u8>,
    configuration: u8,
    pending_configuration: Option<u8>,
    remote_wakeup_enabled: bool,
    interrupt_in_halted: bool,
    protocol: u8,
    idle_rate: u8,
    leds: u8,
    ep0: Ep0Control,

    modifiers: u8,
    pressed_keys: Vec<u8>,
    last_report: [u8; 8],
    pending_reports: VecDeque<[u8; 8]>,
}

impl UsbHidKeyboard {
    pub fn new() -> Self {
        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            remote_wakeup_enabled: false,
            interrupt_in_halted: false,
            protocol: 1, // Report protocol by default.
            idle_rate: 0,
            leds: 0,
            ep0: Ep0Control::new(),
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
        if let Some(bit) = keyboard_modifier_bit(usage) {
            let before = self.modifiers;
            if pressed {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
            changed = before != self.modifiers;
        } else if pressed {
            if !self.pressed_keys.iter().any(|&k| k == usage) {
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

    fn enqueue_current_report(&mut self) {
        let report = build_keyboard_report(self.modifiers, &self.pressed_keys).to_bytes();
        if report == self.last_report {
            return;
        }
        self.last_report = report;
        self.pending_reports.push_back(report);
        if self.pending_reports.len() > MAX_PENDING_REPORTS_KEYBOARD {
            self.pending_reports.pop_front();
        }
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
            if self.configuration == 0 {
                self.modifiers = 0;
                self.pressed_keys.clear();
                self.last_report = [0; 8];
                self.pending_reports.clear();
            }
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
            0x05, 0x07, // Usage Page (Keyboard/Keypad)
            0x19, 0xE0, // Usage Minimum (Left Control)
            0x29, 0xE7, // Usage Maximum (Right GUI)
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
            (0x80, REQ_GET_STATUS) => {
                let mut status = 0u16;
                if self.remote_wakeup_enabled {
                    status |= 1 << 1;
                }
                Some(status.to_le_bytes().to_vec())
            }
            (0x81, REQ_GET_STATUS) => Some(vec![0, 0]),
            (0x82, REQ_GET_STATUS) => {
                if setup.index == 0x81 {
                    let status: u16 = if self.interrupt_in_halted { 1 } else { 0 };
                    Some(status.to_le_bytes().to_vec())
                } else {
                    None
                }
            }
            (0x81, REQ_GET_INTERFACE) => {
                ((setup.index & 0xFF) == 0).then_some(vec![0u8])
            }
            (0xA1, REQ_HID_GET_REPORT) => {
                let report_type = (setup.value >> 8) as u8;
                match report_type {
                    1 => {
                        Some(
                            build_keyboard_report(self.modifiers, &self.pressed_keys)
                                .to_bytes()
                                .to_vec(),
                        )
                    }
                    2 => Some(vec![self.leds]),
                    _ => None,
                }
            }
            (0xA1, REQ_HID_GET_PROTOCOL) => Some(vec![self.protocol]),
            (0xA1, REQ_HID_GET_IDLE) => Some(vec![self.idle_rate]),
            _ => None,
        }
    }

    fn handle_no_data_request(&mut self, setup: SetupPacket) -> bool {
        match (setup.request_type, setup.request) {
            (0x00, REQ_SET_ADDRESS) => {
                if setup.value > 127 {
                    return false;
                }
                self.pending_address = Some((setup.value & 0x7F) as u8);
                true
            }
            (0x00, REQ_SET_CONFIGURATION) => {
                let cfg = (setup.value & 0xFF) as u8;
                if cfg > 1 {
                    return false;
                }
                self.pending_configuration = Some(cfg);
                true
            }
            (0x00, REQ_CLEAR_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = false;
                    true
                } else {
                    false
                }
            }
            (0x00, REQ_SET_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = true;
                    true
                } else {
                    false
                }
            }
            (0x01, REQ_SET_INTERFACE) => setup.value == 0 && (setup.index & 0xFF) == 0,
            (0x02, REQ_CLEAR_FEATURE) => {
                if setup.value == FEATURE_ENDPOINT_HALT && setup.index == 0x81 {
                    self.interrupt_in_halted = false;
                    true
                } else {
                    false
                }
            }
            (0x02, REQ_SET_FEATURE) => {
                if setup.value == FEATURE_ENDPOINT_HALT && setup.index == 0x81 {
                    self.interrupt_in_halted = true;
                    true
                } else {
                    false
                }
            }
            (0x21, REQ_HID_SET_IDLE) => {
                self.idle_rate = (setup.value >> 8) as u8;
                true
            }
            (0x21, REQ_HID_SET_PROTOCOL) => {
                self.protocol = (setup.value & 0xFF) as u8;
                true
            }
            _ => false,
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
        self.remote_wakeup_enabled = false;
        self.interrupt_in_halted = false;
        self.protocol = 1;
        self.idle_rate = 0;
        self.leds = 0;
        self.ep0 = Ep0Control::new();
        self.modifiers = 0;
        self.pressed_keys.clear();
        self.last_report = [0; 8];
        self.pending_reports.clear();
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: SetupPacket) {
        self.ep0.begin(setup);

        let supported = if setup.length == 0 {
            self.handle_no_data_request(setup)
        } else if setup.request_type & 0x80 != 0 {
            if let Some(mut data) = self.handle_setup_inner(setup) {
                data.truncate(setup.length as usize);
                self.ep0.in_data = data;
                true
            } else {
                false
            }
        } else {
            // OUT requests with data stage: support SET_REPORT for LED/output reports.
            matches!(
                (setup.request_type, setup.request),
                (0x21, REQ_HID_SET_REPORT)
            )
        };

        if !supported {
            self.ep0.stalled = true;
        }
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataOut => {
                self.ep0.out_data.extend_from_slice(data);
                if self.ep0.out_data.len() >= self.ep0.out_expected {
                    let setup = self.ep0.setup();
                    match (setup.request_type, setup.request) {
                        (0x21, REQ_HID_SET_REPORT) => {
                            // Store LED/output report value if present.
                            let report_type = (setup.value >> 8) as u8;
                            if report_type == 2 && !self.ep0.out_data.is_empty() {
                                self.leds = self.ep0.out_data[0];
                            }
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
            if self.configuration == 0 {
                return UsbHandshake::Nak;
            }
            if self.interrupt_in_halted {
                return UsbHandshake::Stall;
            }
            let Some(report) = self.pending_reports.pop_front() else {
                return UsbHandshake::Nak;
            };
            // Keyboard boot protocol reports are 8 bytes and match the report
            // descriptor used here.
            let len = buf.len().min(report.len());
            buf[..len].copy_from_slice(&report[..len]);
            return UsbHandshake::Ack { bytes: len };
        }

        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
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
    remote_wakeup_enabled: bool,
    interrupt_in_halted: bool,
    protocol: u8,
    idle_rate: u8,
    ep0: Ep0Control,

    buttons: u8,
    dx: i32,
    dy: i32,
    wheel: i32,
    pending_reports: VecDeque<[u8; 4]>,
}

impl UsbHidMouse {
    pub fn new() -> Self {
        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            remote_wakeup_enabled: false,
            interrupt_in_halted: false,
            protocol: 1,
            idle_rate: 0,
            ep0: Ep0Control::new(),
            buttons: 0,
            dx: 0,
            dy: 0,
            wheel: 0,
            pending_reports: VecDeque::new(),
        }
    }

    pub fn movement(&mut self, dx: i32, dy: i32) {
        self.dx += dx;
        self.dy += dy;
        self.flush_motion();
    }

    pub fn button_event(&mut self, button_mask: u8, pressed: bool) {
        self.flush_motion();
        if pressed {
            self.buttons |= button_mask;
        } else {
            self.buttons &= !button_mask;
        }
        self.push_report([self.buttons & 0x07, 0, 0, 0]);
    }

    pub fn wheel(&mut self, delta: i32) {
        self.wheel += delta;
        self.flush_motion();
    }

    fn flush_motion(&mut self) {
        while self.dx != 0 || self.dy != 0 || self.wheel != 0 {
            let step_x = self.dx.clamp(-127, 127) as i8;
            let step_y = self.dy.clamp(-127, 127) as i8;
            let step_wheel = self.wheel.clamp(-127, 127) as i8;

            self.dx -= step_x as i32;
            self.dy -= step_y as i32;
            self.wheel -= step_wheel as i32;

            self.push_report([
                self.buttons & 0x07,
                step_x as u8,
                step_y as u8,
                step_wheel as u8,
            ]);
        }
    }

    fn push_report(&mut self, report: [u8; 4]) {
        self.pending_reports.push_back(report);
        if self.pending_reports.len() > MAX_PENDING_REPORTS_MOUSE {
            self.pending_reports.pop_front();
        }
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
            if self.configuration == 0 {
                self.pending_reports.clear();
            }
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
            0x05, 0x09, //     Usage Page (Buttons)
            0x19, 0x01, //     Usage Minimum (Button 1)
            0x29, 0x03, //     Usage Maximum (Button 3)
            0x15, 0x00, //     Logical Minimum (0)
            0x25, 0x01, //     Logical Maximum (1)
            0x95, 0x03, //     Report Count (3)
            0x75, 0x01, //     Report Size (1)
            0x81, 0x02, //     Input (Data,Var,Abs) Button bits
            0x95, 0x01, //     Report Count (1)
            0x75, 0x05, //     Report Size (5)
            0x81, 0x01, //     Input (Const,Array,Abs) Padding
            0x05, 0x01, //     Usage Page (Generic Desktop)
            0x09, 0x30, //     Usage (X)
            0x09, 0x31, //     Usage (Y)
            0x09, 0x38, //     Usage (Wheel)
            0x15, 0x81, //     Logical Minimum (-127)
            0x25, 0x7F, //     Logical Maximum (127)
            0x75, 0x08, //     Report Size (8)
            0x95, 0x03, //     Report Count (3)
            0x81, 0x06, //     Input (Data,Var,Rel) X,Y,Wheel
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
                4,
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
            (0x80, REQ_GET_STATUS) => {
                let mut status = 0u16;
                if self.remote_wakeup_enabled {
                    status |= 1 << 1;
                }
                Some(status.to_le_bytes().to_vec())
            }
            (0x81, REQ_GET_STATUS) => Some(vec![0, 0]),
            (0x82, REQ_GET_STATUS) => {
                if setup.index == 0x81 {
                    let status: u16 = if self.interrupt_in_halted { 1 } else { 0 };
                    Some(status.to_le_bytes().to_vec())
                } else {
                    None
                }
            }
            (0x81, REQ_GET_INTERFACE) => {
                ((setup.index & 0xFF) == 0).then_some(vec![0u8])
            }
            (0xA1, REQ_HID_GET_REPORT) => {
                let report_type = (setup.value >> 8) as u8;
                match report_type {
                    1 => Some(vec![self.buttons & 0x07, 0, 0, 0]),
                    _ => None,
                }
            }
            (0xA1, REQ_HID_GET_PROTOCOL) => Some(vec![self.protocol]),
            (0xA1, REQ_HID_GET_IDLE) => Some(vec![self.idle_rate]),
            _ => None,
        }
    }

    fn handle_no_data_request(&mut self, setup: SetupPacket) -> bool {
        match (setup.request_type, setup.request) {
            (0x00, REQ_SET_ADDRESS) => {
                if setup.value > 127 {
                    return false;
                }
                self.pending_address = Some((setup.value & 0x7F) as u8);
                true
            }
            (0x00, REQ_SET_CONFIGURATION) => {
                let cfg = (setup.value & 0xFF) as u8;
                if cfg > 1 {
                    return false;
                }
                self.pending_configuration = Some(cfg);
                true
            }
            (0x00, REQ_CLEAR_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = false;
                    true
                } else {
                    false
                }
            }
            (0x00, REQ_SET_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = true;
                    true
                } else {
                    false
                }
            }
            (0x01, REQ_SET_INTERFACE) => setup.value == 0 && (setup.index & 0xFF) == 0,
            (0x02, REQ_CLEAR_FEATURE) => {
                if setup.value == FEATURE_ENDPOINT_HALT && setup.index == 0x81 {
                    self.interrupt_in_halted = false;
                    true
                } else {
                    false
                }
            }
            (0x02, REQ_SET_FEATURE) => {
                if setup.value == FEATURE_ENDPOINT_HALT && setup.index == 0x81 {
                    self.interrupt_in_halted = true;
                    true
                } else {
                    false
                }
            }
            (0x21, REQ_HID_SET_IDLE) => {
                self.idle_rate = (setup.value >> 8) as u8;
                true
            }
            (0x21, REQ_HID_SET_PROTOCOL) => {
                self.protocol = (setup.value & 0xFF) as u8;
                true
            }
            _ => false,
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
        self.remote_wakeup_enabled = false;
        self.interrupt_in_halted = false;
        self.protocol = 1;
        self.idle_rate = 0;
        self.ep0 = Ep0Control::new();
        self.buttons = 0;
        self.dx = 0;
        self.dy = 0;
        self.wheel = 0;
        self.pending_reports.clear();
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: SetupPacket) {
        self.ep0.begin(setup);

        let supported = if setup.length == 0 {
            self.handle_no_data_request(setup)
        } else if setup.request_type & 0x80 != 0 {
            if let Some(mut data) = self.handle_setup_inner(setup) {
                data.truncate(setup.length as usize);
                self.ep0.in_data = data;
                true
            } else {
                false
            }
        } else {
            matches!(
                (setup.request_type, setup.request),
                (0x21, REQ_HID_SET_REPORT)
            )
        };

        if !supported {
            self.ep0.stalled = true;
        }
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataOut => {
                self.ep0.out_data.extend_from_slice(data);
                if self.ep0.out_data.len() >= self.ep0.out_expected {
                    let setup = self.ep0.setup();
                    match (setup.request_type, setup.request) {
                        (0x21, REQ_HID_SET_REPORT) => {}
                        _ => {
                            let _ = self.handle_no_data_request(setup);
                        }
                    }
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
            if self.configuration == 0 {
                return UsbHandshake::Nak;
            }
            if self.interrupt_in_halted {
                return UsbHandshake::Stall;
            }
            let Some(report) = self.pending_reports.pop_front() else {
                return UsbHandshake::Nak;
            };
            // Boot protocol reports are 3 bytes (buttons, X, Y). Report protocol adds a 4th
            // wheel byte.
            let report_len = if self.protocol == 0 { 3 } else { report.len() };
            let len = buf.len().min(report_len);
            buf[..len].copy_from_slice(&report[..len]);
            return UsbHandshake::Ack { bytes: len };
        }

        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
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

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct GamepadReport {
    pub buttons: u16,
    /// Hat switch value (low 4 bits). `8` is used as the null/centered state.
    pub hat: u8,
    pub x: i8,
    pub y: i8,
    pub rx: i8,
    pub ry: i8,
}

impl GamepadReport {
    pub fn empty() -> Self {
        Self {
            buttons: 0,
            hat: 8,
            x: 0,
            y: 0,
            rx: 0,
            ry: 0,
        }
    }

    fn to_bytes(self) -> [u8; 8] {
        let [b0, b1] = self.buttons.to_le_bytes();
        [
            b0,
            b1,
            self.hat & 0x0F,
            self.x as u8,
            self.y as u8,
            self.rx as u8,
            self.ry as u8,
            0x00,
        ]
    }
}

pub struct UsbHidGamepad {
    address: u8,
    pending_address: Option<u8>,
    configuration: u8,
    pending_configuration: Option<u8>,
    remote_wakeup_enabled: bool,
    interrupt_in_halted: bool,
    protocol: u8,
    idle_rate: u8,
    ep0: Ep0Control,

    report: GamepadReport,
    pending_reports: VecDeque<[u8; 8]>,
}

impl UsbHidGamepad {
    pub fn new() -> Self {
        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            remote_wakeup_enabled: false,
            interrupt_in_halted: false,
            protocol: 1,
            idle_rate: 0,
            ep0: Ep0Control::new(),
            report: GamepadReport::empty(),
            pending_reports: VecDeque::new(),
        }
    }

    /// Overwrite the current report state and enqueue exactly one report.
    pub fn set_report(&mut self, mut report: GamepadReport) {
        report.hat &= 0x0F;
        if report.hat > 8 {
            report.hat = 8;
        }
        self.report = report;
        self.enqueue_report();
    }

    /// Set or clear a gamepad button.
    ///
    /// `button_idx` is 1-based and maps directly to HID usages Button 1..16.
    pub fn button_event(&mut self, button_idx: u8, pressed: bool) {
        if !(1..=16).contains(&button_idx) {
            return;
        }
        let bit = 1u16 << (button_idx - 1);
        if pressed {
            self.report.buttons |= bit;
        } else {
            self.report.buttons &= !bit;
        }
        self.enqueue_report();
    }

    pub fn set_buttons(&mut self, buttons: u16) {
        self.report.buttons = buttons;
        self.enqueue_report();
    }

    /// Sets the hat switch direction.
    ///
    /// - `None` means centered (null state).
    /// - `Some(0..=7)` corresponds to N, NE, E, SE, S, SW, W, NW.
    pub fn set_hat(&mut self, hat: Option<u8>) {
        self.report.hat = match hat {
            Some(v) if v <= 7 => v,
            _ => 8,
        };
        self.enqueue_report();
    }

    pub fn set_axes(&mut self, x: i32, y: i32) {
        self.report.x = x.clamp(-127, 127) as i8;
        self.report.y = y.clamp(-127, 127) as i8;
        self.enqueue_report();
    }

    pub fn set_axes_full(&mut self, x: i32, y: i32, rx: i32, ry: i32) {
        self.report.x = x.clamp(-127, 127) as i8;
        self.report.y = y.clamp(-127, 127) as i8;
        self.report.rx = rx.clamp(-127, 127) as i8;
        self.report.ry = ry.clamp(-127, 127) as i8;
        self.enqueue_report();
    }

    fn enqueue_report(&mut self) {
        self.pending_reports.push_back(self.report.to_bytes());
        if self.pending_reports.len() > MAX_PENDING_REPORTS_GAMEPAD {
            self.pending_reports.pop_front();
        }
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
            if self.configuration == 0 {
                self.pending_reports.clear();
            }
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
            0x03,
            0x00,
            0x00,
            0x01,
            1,
            4,
            0,
            1,
        ];
        &DESC
    }

    fn report_descriptor() -> &'static [u8] {
        static DESC: &[u8] = &[
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x05, // Usage (Game Pad)
            0xA1, 0x01, // Collection (Application)
            0x05, 0x09, // Usage Page (Button)
            0x19, 0x01, // Usage Minimum (Button 1)
            0x29, 0x10, // Usage Maximum (Button 16)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x01, // Logical Maximum (1)
            0x75, 0x01, // Report Size (1)
            0x95, 0x10, // Report Count (16)
            0x81, 0x02, // Input (Data,Var,Abs) Buttons
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x39, // Usage (Hat switch)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x07, // Logical Maximum (7)
            0x35, 0x00, // Physical Minimum (0)
            0x46, 0x3B, 0x01, // Physical Maximum (315)
            0x65, 0x14, // Unit (Eng Rot: Degrees)
            0x75, 0x04, // Report Size (4)
            0x95, 0x01, // Report Count (1)
            0x81, 0x42, // Input (Data,Var,Abs,Null) Hat
            0x65, 0x00, // Unit (None)
            0x75, 0x04, // Report Size (4)
            0x95, 0x01, // Report Count (1)
            0x81, 0x01, // Input (Const,Array,Abs) Padding
            0x09, 0x30, // Usage (X)
            0x09, 0x31, // Usage (Y)
            0x09, 0x33, // Usage (Rx)
            0x09, 0x34, // Usage (Ry)
            0x15, 0x81, // Logical Minimum (-127)
            0x25, 0x7F, // Logical Maximum (127)
            0x75, 0x08, // Report Size (8)
            0x95, 0x04, // Report Count (4)
            0x81, 0x02, // Input (Data,Var,Abs) Axes
            0x75, 0x08, // Report Size (8)
            0x95, 0x01, // Report Count (1)
            0x81, 0x01, // Input (Const,Array,Abs) Padding
            0xC0, // End Collection
        ];
        DESC
    }

    fn configuration_descriptor() -> &'static [u8] {
        static DESC: [u8; 34] = {
            let [rl0, rl1] = GAMEPAD_REPORT_DESCRIPTOR_LEN.to_le_bytes();
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
                0x00,
                0x00,
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
                8,
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
                4 => Some(string_descriptor_utf16le("Aero HID Gamepad")),
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
            (0x80, REQ_GET_STATUS) => {
                let mut status = 0u16;
                if self.remote_wakeup_enabled {
                    status |= 1 << 1;
                }
                Some(status.to_le_bytes().to_vec())
            }
            (0x81, REQ_GET_STATUS) => Some(vec![0, 0]),
            (0x82, REQ_GET_STATUS) => {
                if setup.index == 0x81 {
                    let status: u16 = if self.interrupt_in_halted { 1 } else { 0 };
                    Some(status.to_le_bytes().to_vec())
                } else {
                    None
                }
            }
            (0x81, REQ_GET_INTERFACE) => {
                ((setup.index & 0xFF) == 0).then_some(vec![0u8])
            }
            (0xA1, REQ_HID_GET_REPORT) => {
                let report_type = (setup.value >> 8) as u8;
                match report_type {
                    1 => Some(self.report.to_bytes().to_vec()),
                    _ => None,
                }
            }
            (0xA1, REQ_HID_GET_PROTOCOL) => Some(vec![self.protocol]),
            (0xA1, REQ_HID_GET_IDLE) => Some(vec![self.idle_rate]),
            _ => None,
        }
    }

    fn handle_no_data_request(&mut self, setup: SetupPacket) -> bool {
        match (setup.request_type, setup.request) {
            (0x00, REQ_SET_ADDRESS) => {
                if setup.value > 127 {
                    return false;
                }
                self.pending_address = Some((setup.value & 0x7F) as u8);
                true
            }
            (0x00, REQ_SET_CONFIGURATION) => {
                let cfg = (setup.value & 0xFF) as u8;
                if cfg > 1 {
                    return false;
                }
                self.pending_configuration = Some(cfg);
                true
            }
            (0x00, REQ_CLEAR_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = false;
                    true
                } else {
                    false
                }
            }
            (0x00, REQ_SET_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = true;
                    true
                } else {
                    false
                }
            }
            (0x01, REQ_SET_INTERFACE) => setup.value == 0 && (setup.index & 0xFF) == 0,
            (0x02, REQ_CLEAR_FEATURE) => {
                if setup.value == FEATURE_ENDPOINT_HALT && setup.index == 0x81 {
                    self.interrupt_in_halted = false;
                    true
                } else {
                    false
                }
            }
            (0x02, REQ_SET_FEATURE) => {
                if setup.value == FEATURE_ENDPOINT_HALT && setup.index == 0x81 {
                    self.interrupt_in_halted = true;
                    true
                } else {
                    false
                }
            }
            (0x21, REQ_HID_SET_IDLE) => {
                self.idle_rate = (setup.value >> 8) as u8;
                true
            }
            (0x21, REQ_HID_SET_PROTOCOL) => {
                self.protocol = (setup.value & 0xFF) as u8;
                true
            }
            _ => false,
        }
    }
}

impl Default for UsbHidGamepad {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDevice for UsbHidGamepad {
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
        self.remote_wakeup_enabled = false;
        self.interrupt_in_halted = false;
        self.protocol = 1;
        self.idle_rate = 0;
        self.ep0 = Ep0Control::new();
        self.report = GamepadReport::empty();
        self.pending_reports.clear();
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: SetupPacket) {
        self.ep0.begin(setup);

        let supported = if setup.length == 0 {
            self.handle_no_data_request(setup)
        } else if setup.request_type & 0x80 != 0 {
            if let Some(mut data) = self.handle_setup_inner(setup) {
                data.truncate(setup.length as usize);
                self.ep0.in_data = data;
                true
            } else {
                false
            }
        } else {
            matches!(
                (setup.request_type, setup.request),
                (0x21, REQ_HID_SET_REPORT)
            )
        };

        if !supported {
            self.ep0.stalled = true;
        }
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataOut => {
                self.ep0.out_data.extend_from_slice(data);
                if self.ep0.out_data.len() >= self.ep0.out_expected {
                    let setup = self.ep0.setup();
                    if matches!((setup.request_type, setup.request), (0x21, REQ_HID_SET_REPORT)) {
                        // Ignore output reports.
                    } else {
                        let _ = self.handle_no_data_request(setup);
                    }
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
            if self.configuration == 0 {
                return UsbHandshake::Nak;
            }
            if self.interrupt_in_halted {
                return UsbHandshake::Stall;
            }
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
        if self.ep0.stalled {
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

pub struct UsbHidCompositeInput {
    address: u8,
    pending_address: Option<u8>,
    configuration: u8,
    pending_configuration: Option<u8>,
    remote_wakeup_enabled: bool,
    interrupt_in_halted: [bool; 3],
    protocols: [u8; 3],
    idle_rates: [u8; 3],
    ep0: Ep0Control,

    keyboard_modifiers: u8,
    keyboard_pressed_keys: Vec<u8>,
    keyboard_last_report: [u8; 8],
    keyboard_leds: u8,
    pending_keyboard_reports: VecDeque<[u8; 8]>,

    mouse_buttons: u8,
    pending_mouse_reports: VecDeque<[u8; 4]>,

    gamepad_report: GamepadReport,
    pending_gamepad_reports: VecDeque<[u8; 8]>,
}

impl UsbHidCompositeInput {
    pub fn new() -> Self {
        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            remote_wakeup_enabled: false,
            interrupt_in_halted: [false; 3],
            protocols: [1; 3],
            idle_rates: [0; 3],
            ep0: Ep0Control::new(),
            keyboard_modifiers: 0,
            keyboard_pressed_keys: Vec::new(),
            keyboard_last_report: [0; 8],
            keyboard_leds: 0,
            pending_keyboard_reports: VecDeque::new(),
            mouse_buttons: 0,
            pending_mouse_reports: VecDeque::new(),
            gamepad_report: GamepadReport::empty(),
            pending_gamepad_reports: VecDeque::new(),
        }
    }

    pub fn key_event(&mut self, usage: u8, pressed: bool) {
        if usage == 0 {
            return;
        }

        let mut changed = false;
        if let Some(bit) = keyboard_modifier_bit(usage) {
            let before = self.keyboard_modifiers;
            if pressed {
                self.keyboard_modifiers |= bit;
            } else {
                self.keyboard_modifiers &= !bit;
            }
            changed = before != self.keyboard_modifiers;
        } else if pressed {
            if !self.keyboard_pressed_keys.iter().any(|&k| k == usage) {
                self.keyboard_pressed_keys.push(usage);
                changed = true;
            }
        } else {
            let before_len = self.keyboard_pressed_keys.len();
            self.keyboard_pressed_keys.retain(|&k| k != usage);
            changed = before_len != self.keyboard_pressed_keys.len();
        }

        if changed {
            self.enqueue_keyboard_report();
        }
    }

    pub fn mouse_movement(&mut self, dx: i32, dy: i32) {
        let dx = dx.clamp(-127, 127) as i8 as u8;
        let dy = dy.clamp(-127, 127) as i8 as u8;
        self.push_mouse_report([self.mouse_buttons & 0x07, dx, dy, 0]);
    }

    pub fn mouse_button_event(&mut self, button_mask: u8, pressed: bool) {
        if pressed {
            self.mouse_buttons |= button_mask;
        } else {
            self.mouse_buttons &= !button_mask;
        }
        self.push_mouse_report([self.mouse_buttons & 0x07, 0, 0, 0]);
    }

    pub fn mouse_wheel(&mut self, delta: i32) {
        let wheel = delta.clamp(-127, 127) as i8 as u8;
        self.push_mouse_report([self.mouse_buttons & 0x07, 0, 0, wheel]);
    }

    fn push_mouse_report(&mut self, report: [u8; 4]) {
        self.pending_mouse_reports.push_back(report);
        if self.pending_mouse_reports.len() > MAX_PENDING_REPORTS_MOUSE {
            self.pending_mouse_reports.pop_front();
        }
    }

    pub fn gamepad_button_event(&mut self, button_idx: u8, pressed: bool) {
        if !(1..=16).contains(&button_idx) {
            return;
        }
        let bit = 1u16 << (button_idx - 1);
        if pressed {
            self.gamepad_report.buttons |= bit;
        } else {
            self.gamepad_report.buttons &= !bit;
        }
        self.enqueue_gamepad_report();
    }

    pub fn gamepad_axes(&mut self, x: i32, y: i32) {
        self.gamepad_report.x = x.clamp(-127, 127) as i8;
        self.gamepad_report.y = y.clamp(-127, 127) as i8;
        self.enqueue_gamepad_report();
    }

    pub fn gamepad_axes_full(&mut self, x: i32, y: i32, rx: i32, ry: i32) {
        self.gamepad_report.x = x.clamp(-127, 127) as i8;
        self.gamepad_report.y = y.clamp(-127, 127) as i8;
        self.gamepad_report.rx = rx.clamp(-127, 127) as i8;
        self.gamepad_report.ry = ry.clamp(-127, 127) as i8;
        self.enqueue_gamepad_report();
    }

    fn enqueue_gamepad_report(&mut self) {
        self.pending_gamepad_reports
            .push_back(self.gamepad_report.to_bytes());
        if self.pending_gamepad_reports.len() > MAX_PENDING_REPORTS_GAMEPAD {
            self.pending_gamepad_reports.pop_front();
        }
    }

    fn enqueue_keyboard_report(&mut self) {
        let report = build_keyboard_report(self.keyboard_modifiers, &self.keyboard_pressed_keys).to_bytes();
        if report == self.keyboard_last_report {
            return;
        }
        self.keyboard_last_report = report;
        self.pending_keyboard_reports.push_back(report);
        if self.pending_keyboard_reports.len() > MAX_PENDING_REPORTS_KEYBOARD {
            self.pending_keyboard_reports.pop_front();
        }
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
            if self.configuration == 0 {
                self.keyboard_modifiers = 0;
                self.keyboard_pressed_keys.clear();
                self.keyboard_last_report = [0; 8];
                self.pending_keyboard_reports.clear();
                self.pending_mouse_reports.clear();
                self.pending_gamepad_reports.clear();
            }
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
            0x04,
            0x00,
            0x00,
            0x01,
            1,
            5,
            0,
            1,
        ];
        &DESC
    }

    fn configuration_descriptor() -> &'static [u8] {
        static DESC: [u8; 84] = {
            let [krl0, krl1] = KEYBOARD_REPORT_DESCRIPTOR_LEN.to_le_bytes();
            let [mrl0, mrl1] = MOUSE_REPORT_DESCRIPTOR_LEN.to_le_bytes();
            let [grl0, grl1] = GAMEPAD_REPORT_DESCRIPTOR_LEN.to_le_bytes();
            let [tl0, tl1] = (84u16).to_le_bytes();
            [
                // Configuration descriptor.
                9,
                DESC_CONFIGURATION,
                tl0,
                tl1,
                3,
                1,
                0,
                0xA0,
                50,
                // Interface 0: Keyboard.
                9,
                0x04,
                0,
                0,
                1,
                0x03,
                0x01,
                0x01,
                0,
                9,
                DESC_HID,
                0x11,
                0x01,
                0,
                1,
                DESC_REPORT,
                krl0,
                krl1,
                7,
                0x05,
                0x81,
                0x03,
                8,
                0,
                10,
                // Interface 1: Mouse.
                9,
                0x04,
                1,
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
                mrl0,
                mrl1,
                7,
                0x05,
                0x82,
                0x03,
                4,
                0,
                10,
                // Interface 2: Gamepad.
                9,
                0x04,
                2,
                0,
                1,
                0x03,
                0x00,
                0x00,
                0,
                9,
                DESC_HID,
                0x11,
                0x01,
                0,
                1,
                DESC_REPORT,
                grl0,
                grl1,
                7,
                0x05,
                0x83,
                0x03,
                8,
                0,
                10,
            ]
        };
        &DESC
    }

    fn report_descriptor_keyboard() -> &'static [u8] {
        UsbHidKeyboard::report_descriptor()
    }

    fn report_descriptor_mouse() -> &'static [u8] {
        UsbHidMouse::report_descriptor()
    }

    fn report_descriptor_gamepad() -> &'static [u8] {
        UsbHidGamepad::report_descriptor()
    }

    fn hid_descriptor_for_interface(interface: u8) -> Option<&'static [u8]> {
        let offset = match interface {
            0 => 18,
            1 => 43,
            2 => 68,
            _ => return None,
        };
        Some(&Self::configuration_descriptor()[offset..offset + 9])
    }

    fn get_descriptor(&self, desc_type: u8, index: u8, interface: u8) -> Option<Vec<u8>> {
        match desc_type {
            DESC_DEVICE => Some(Self::device_descriptor().to_vec()),
            DESC_CONFIGURATION => Some(Self::configuration_descriptor().to_vec()),
            DESC_STRING => match index {
                0 => Some(string_descriptor_langid(0x0409).to_vec()),
                1 => Some(string_descriptor_utf16le("Aero")),
                5 => Some(string_descriptor_utf16le("Aero HID Composite Input")),
                _ => Some(vec![0, DESC_STRING]),
            },
            DESC_HID => Self::hid_descriptor_for_interface(interface).map(|v| v.to_vec()),
            DESC_REPORT => match interface {
                0 => Some(Self::report_descriptor_keyboard().to_vec()),
                1 => Some(Self::report_descriptor_mouse().to_vec()),
                2 => Some(Self::report_descriptor_gamepad().to_vec()),
                _ => None,
            },
            _ => None,
        }
    }

    fn handle_setup_inner(&mut self, setup: SetupPacket) -> Option<Vec<u8>> {
        let interface = (setup.index & 0xFF) as u8;
        match (setup.request_type, setup.request) {
            (0x80, REQ_GET_DESCRIPTOR) | (0x81, REQ_GET_DESCRIPTOR) => {
                let desc_type = (setup.value >> 8) as u8;
                let index = (setup.value & 0xFF) as u8;
                self.get_descriptor(desc_type, index, interface)
            }
            (0x80, REQ_GET_CONFIGURATION) => Some(vec![self.configuration]),
            (0x80, REQ_GET_STATUS) => {
                let mut status = 0u16;
                if self.remote_wakeup_enabled {
                    status |= 1 << 1;
                }
                Some(status.to_le_bytes().to_vec())
            }
            (0x81, REQ_GET_STATUS) => ((setup.index & 0xFF) <= 2).then_some(vec![0, 0]),
            (0x82, REQ_GET_STATUS) => {
                let halted = match (setup.index & 0xFF) as u8 {
                    0x81 => self.interrupt_in_halted[0],
                    0x82 => self.interrupt_in_halted[1],
                    0x83 => self.interrupt_in_halted[2],
                    _ => return None,
                };
                let status: u16 = if halted { 1 } else { 0 };
                Some(status.to_le_bytes().to_vec())
            }
            (0x81, REQ_GET_INTERFACE) => {
                ((setup.index & 0xFF) <= 2).then_some(vec![0u8])
            }
            (0xA1, REQ_HID_GET_REPORT) => {
                let report_type = (setup.value >> 8) as u8;
                match (interface, report_type) {
                    (0, 1) => {
                        Some(
                            build_keyboard_report(self.keyboard_modifiers, &self.keyboard_pressed_keys)
                                .to_bytes()
                                .to_vec(),
                        )
                    }
                    (0, 2) => Some(vec![self.keyboard_leds]),
                    (1, 1) => {
                        if self.protocols[1] == 0 {
                            Some(vec![self.mouse_buttons & 0x07, 0, 0])
                        } else {
                            Some(vec![self.mouse_buttons & 0x07, 0, 0, 0])
                        }
                    }
                    (2, 1) => Some(self.gamepad_report.to_bytes().to_vec()),
                    _ => None,
                }
            }
            (0xA1, REQ_HID_GET_PROTOCOL) => self.protocols.get(interface as usize).copied().map(|v| vec![v]),
            (0xA1, REQ_HID_GET_IDLE) => self.idle_rates.get(interface as usize).copied().map(|v| vec![v]),
            _ => None,
        }
    }

    fn handle_no_data_request(&mut self, setup: SetupPacket) -> bool {
        let interface = (setup.index & 0xFF) as usize;
        match (setup.request_type, setup.request) {
            (0x00, REQ_SET_ADDRESS) => {
                if setup.value > 127 {
                    return false;
                }
                self.pending_address = Some((setup.value & 0x7F) as u8);
                true
            }
            (0x00, REQ_SET_CONFIGURATION) => {
                let cfg = (setup.value & 0xFF) as u8;
                if cfg > 1 {
                    return false;
                }
                self.pending_configuration = Some(cfg);
                true
            }
            (0x00, REQ_CLEAR_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = false;
                    true
                } else {
                    false
                }
            }
            (0x00, REQ_SET_FEATURE) => {
                if setup.value == FEATURE_DEVICE_REMOTE_WAKEUP {
                    self.remote_wakeup_enabled = true;
                    true
                } else {
                    false
                }
            }
            (0x01, REQ_SET_INTERFACE) => setup.value == 0 && (setup.index & 0xFF) <= 2,
            (0x02, REQ_CLEAR_FEATURE) => {
                if setup.value != FEATURE_ENDPOINT_HALT {
                    return false;
                }
                match (setup.index & 0xFF) as u8 {
                    0x81 => self.interrupt_in_halted[0] = false,
                    0x82 => self.interrupt_in_halted[1] = false,
                    0x83 => self.interrupt_in_halted[2] = false,
                    _ => return false,
                }
                true
            }
            (0x02, REQ_SET_FEATURE) => {
                if setup.value != FEATURE_ENDPOINT_HALT {
                    return false;
                }
                match (setup.index & 0xFF) as u8 {
                    0x81 => self.interrupt_in_halted[0] = true,
                    0x82 => self.interrupt_in_halted[1] = true,
                    0x83 => self.interrupt_in_halted[2] = true,
                    _ => return false,
                }
                true
            }
            (0x21, REQ_HID_SET_IDLE) => {
                if let Some(rate) = self.idle_rates.get_mut(interface) {
                    *rate = (setup.value >> 8) as u8;
                    true
                } else {
                    false
                }
            }
            (0x21, REQ_HID_SET_PROTOCOL) => {
                if let Some(proto) = self.protocols.get_mut(interface) {
                    *proto = (setup.value & 0xFF) as u8;
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

impl Default for UsbHidCompositeInput {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDevice for UsbHidCompositeInput {
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
        self.remote_wakeup_enabled = false;
        self.interrupt_in_halted = [false; 3];
        self.protocols = [1; 3];
        self.idle_rates = [0; 3];
        self.ep0 = Ep0Control::new();
        self.keyboard_modifiers = 0;
        self.keyboard_pressed_keys.clear();
        self.keyboard_last_report = [0; 8];
        self.keyboard_leds = 0;
        self.pending_keyboard_reports.clear();
        self.mouse_buttons = 0;
        self.pending_mouse_reports.clear();
        self.gamepad_report = GamepadReport::empty();
        self.pending_gamepad_reports.clear();
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: SetupPacket) {
        self.ep0.begin(setup);

        let supported = if setup.length == 0 {
            self.handle_no_data_request(setup)
        } else if setup.request_type & 0x80 != 0 {
            if let Some(mut data) = self.handle_setup_inner(setup) {
                data.truncate(setup.length as usize);
                self.ep0.in_data = data;
                true
            } else {
                false
            }
        } else {
            matches!(
                (setup.request_type, setup.request),
                (0x21, REQ_HID_SET_REPORT)
            )
        };

        if !supported {
            self.ep0.stalled = true;
        }
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataOut => {
                self.ep0.out_data.extend_from_slice(data);
                if self.ep0.out_data.len() >= self.ep0.out_expected {
                    let setup = self.ep0.setup();
                    if matches!((setup.request_type, setup.request), (0x21, REQ_HID_SET_REPORT)) {
                        // Store LED/output report value for the keyboard interface if present; keep
                        // the transfer successful regardless.
                        let interface = (setup.index & 0xFF) as u8;
                        let report_type = (setup.value >> 8) as u8;
                        if interface == 0 && report_type == 2 && !self.ep0.out_data.is_empty() {
                            self.keyboard_leds = self.ep0.out_data[0];
                        }
                    } else {
                        let _ = self.handle_no_data_request(setup);
                    }
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
        match ep {
            1 => {
                if self.configuration == 0 {
                    return UsbHandshake::Nak;
                }
                if self.interrupt_in_halted[0] {
                    return UsbHandshake::Stall;
                }
                let Some(report) = self.pending_keyboard_reports.pop_front() else {
                    return UsbHandshake::Nak;
                };
                let len = buf.len().min(report.len());
                buf[..len].copy_from_slice(&report[..len]);
                return UsbHandshake::Ack { bytes: len };
            }
            2 => {
                if self.configuration == 0 {
                    return UsbHandshake::Nak;
                }
                if self.interrupt_in_halted[1] {
                    return UsbHandshake::Stall;
                }
                let Some(report) = self.pending_mouse_reports.pop_front() else {
                    return UsbHandshake::Nak;
                };
                let report_len = if self.protocols[1] == 0 { 3 } else { report.len() };
                let len = buf.len().min(report_len);
                buf[..len].copy_from_slice(&report[..len]);
                return UsbHandshake::Ack { bytes: len };
            }
            3 => {
                if self.configuration == 0 {
                    return UsbHandshake::Nak;
                }
                if self.interrupt_in_halted[2] {
                    return UsbHandshake::Stall;
                }
                let Some(report) = self.pending_gamepad_reports.pop_front() else {
                    return UsbHandshake::Nak;
                };
                let len = buf.len().min(report.len());
                buf[..len].copy_from_slice(&report[..len]);
                return UsbHandshake::Ack { bytes: len };
            }
            _ => {}
        }

        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_stalls_unknown_descriptor_types() {
        let mut kb = UsbHidKeyboard::new();
        kb.handle_setup(SetupPacket {
            request_type: 0x80,
            request: REQ_GET_DESCRIPTOR,
            value: 0x0600, // Device Qualifier (not supported for full-speed only device)
            index: 0,
            length: 10,
        });

        let mut buf = [0u8; 16];
        assert_eq!(kb.handle_in(0, &mut buf), UsbHandshake::Stall);
    }

    #[test]
    fn mouse_stalls_unknown_descriptor_types() {
        let mut mouse = UsbHidMouse::new();
        mouse.handle_setup(SetupPacket {
            request_type: 0x80,
            request: REQ_GET_DESCRIPTOR,
            value: 0x0600,
            index: 0,
            length: 10,
        });

        let mut buf = [0u8; 16];
        assert_eq!(mouse.handle_in(0, &mut buf), UsbHandshake::Stall);
    }

    #[test]
    fn gamepad_stalls_unknown_descriptor_types() {
        let mut gamepad = UsbHidGamepad::new();
        gamepad.handle_setup(SetupPacket {
            request_type: 0x80,
            request: REQ_GET_DESCRIPTOR,
            value: 0x0600,
            index: 0,
            length: 10,
        });

        let mut buf = [0u8; 16];
        assert_eq!(gamepad.handle_in(0, &mut buf), UsbHandshake::Stall);
    }

    #[test]
    fn composite_stalls_unknown_descriptor_types() {
        let mut dev = UsbHidCompositeInput::new();
        dev.handle_setup(SetupPacket {
            request_type: 0x80,
            request: REQ_GET_DESCRIPTOR,
            value: 0x0600,
            index: 0,
            length: 10,
        });

        let mut buf = [0u8; 16];
        assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Stall);
    }

    #[test]
    fn keyboard_get_report_returns_current_state_and_leds() {
        let mut kb = UsbHidKeyboard::new();
        kb.key_event(0x04, true); // 'a'

        kb.handle_setup(SetupPacket {
            request_type: 0xA1,
            request: REQ_HID_GET_REPORT,
            value: 0x0100,
            index: 0,
            length: 8,
        });
        let mut buf = [0u8; 8];
        assert_eq!(kb.handle_in(0, &mut buf), UsbHandshake::Ack { bytes: 8 });
        assert_eq!(buf[2], 0x04);
        assert_eq!(kb.handle_out(0, &[]), UsbHandshake::Ack { bytes: 0 });

        // SET_REPORT(Output) with one byte should update LED state.
        kb.handle_setup(SetupPacket {
            request_type: 0x21,
            request: REQ_HID_SET_REPORT,
            value: 0x0200,
            index: 0,
            length: 1,
        });
        assert_eq!(kb.handle_out(0, &[0x05]), UsbHandshake::Ack { bytes: 1 });
        let mut empty = [0u8; 0];
        assert_eq!(kb.handle_in(0, &mut empty), UsbHandshake::Ack { bytes: 0 });

        kb.handle_setup(SetupPacket {
            request_type: 0xA1,
            request: REQ_HID_GET_REPORT,
            value: 0x0200,
            index: 0,
            length: 1,
        });
        let mut out = [0u8; 1];
        assert_eq!(kb.handle_in(0, &mut out), UsbHandshake::Ack { bytes: 1 });
        assert_eq!(out[0], 0x05);
    }

    #[test]
    fn pending_report_queues_are_bounded() {
        let mut kb = UsbHidKeyboard::new();
        for _ in 0..(MAX_PENDING_REPORTS_KEYBOARD + 32) {
            kb.key_event(0x04, true);
            kb.key_event(0x04, false);
        }
        assert!(kb.pending_reports.len() <= MAX_PENDING_REPORTS_KEYBOARD);

        let mut mouse = UsbHidMouse::new();
        for _ in 0..(MAX_PENDING_REPORTS_MOUSE + 32) {
            mouse.movement(1, 1);
        }
        assert!(mouse.pending_reports.len() <= MAX_PENDING_REPORTS_MOUSE);

        let mut gamepad = UsbHidGamepad::new();
        for i in 0..(MAX_PENDING_REPORTS_GAMEPAD + 32) {
            gamepad.set_axes(i as i32, -(i as i32));
        }
        assert!(gamepad.pending_reports.len() <= MAX_PENDING_REPORTS_GAMEPAD);

        let mut composite = UsbHidCompositeInput::new();
        for _ in 0..(MAX_PENDING_REPORTS_KEYBOARD + 32) {
            composite.key_event(0x04, true);
            composite.key_event(0x04, false);
        }
        assert!(composite.pending_keyboard_reports.len() <= MAX_PENDING_REPORTS_KEYBOARD);

        for _ in 0..(MAX_PENDING_REPORTS_MOUSE + 32) {
            composite.mouse_movement(1, 1);
        }
        assert!(composite.pending_mouse_reports.len() <= MAX_PENDING_REPORTS_MOUSE);

        for i in 0..(MAX_PENDING_REPORTS_GAMEPAD + 32) {
            composite.gamepad_axes(i as i32, -(i as i32));
        }
        assert!(composite.pending_gamepad_reports.len() <= MAX_PENDING_REPORTS_GAMEPAD);
    }

    #[test]
    fn report_descriptor_lengths_match_constants() {
        assert_eq!(
            UsbHidKeyboard::report_descriptor().len(),
            KEYBOARD_REPORT_DESCRIPTOR_LEN as usize
        );
        assert_eq!(
            UsbHidMouse::report_descriptor().len(),
            MOUSE_REPORT_DESCRIPTOR_LEN as usize
        );
        assert_eq!(
            UsbHidGamepad::report_descriptor().len(),
            GAMEPAD_REPORT_DESCRIPTOR_LEN as usize
        );
    }

    #[test]
    fn mouse_boot_protocol_report_is_three_bytes() {
        let mut mouse = UsbHidMouse::new();
        mouse.protocol = 0;

        // Interrupt IN endpoints are only valid once the device is configured.
        mouse.handle_setup(SetupPacket {
            request_type: 0x00,
            request: REQ_SET_CONFIGURATION,
            value: 1,
            index: 0,
            length: 0,
        });
        let mut empty = [0u8; 0];
        assert_eq!(
            mouse.handle_in(0, &mut empty),
            UsbHandshake::Ack { bytes: 0 }
        );

        mouse.movement(1, -2);

        let mut buf = [0u8; 8];
        assert_eq!(
            mouse.handle_in(1, &mut buf),
            UsbHandshake::Ack { bytes: 3 }
        );
        assert_eq!(&buf[..3], &[0x00, 0x01, 0xfe]);
    }

    #[test]
    fn keyboard_standard_status_and_endpoint_halt_bits() {
        let mut kb = UsbHidKeyboard::new();

        // Default: remote wakeup disabled.
        kb.handle_setup(SetupPacket {
            request_type: 0x80,
            request: REQ_GET_STATUS,
            value: 0,
            index: 0,
            length: 2,
        });
        let mut status = [0u8; 2];
        assert_eq!(kb.handle_in(0, &mut status), UsbHandshake::Ack { bytes: 2 });
        assert_eq!(status, [0, 0]);
        assert_eq!(kb.handle_out(0, &[]), UsbHandshake::Ack { bytes: 0 });

        // Enable remote wakeup.
        kb.handle_setup(SetupPacket {
            request_type: 0x00,
            request: REQ_SET_FEATURE,
            value: FEATURE_DEVICE_REMOTE_WAKEUP,
            index: 0,
            length: 0,
        });
        let mut empty = [0u8; 0];
        assert_eq!(kb.handle_in(0, &mut empty), UsbHandshake::Ack { bytes: 0 });

        kb.handle_setup(SetupPacket {
            request_type: 0x80,
            request: REQ_GET_STATUS,
            value: 0,
            index: 0,
            length: 2,
        });
        assert_eq!(kb.handle_in(0, &mut status), UsbHandshake::Ack { bytes: 2 });
        assert_eq!(status, [0x02, 0x00]);
        assert_eq!(kb.handle_out(0, &[]), UsbHandshake::Ack { bytes: 0 });

        // Configure device so interrupt endpoints are enabled.
        kb.handle_setup(SetupPacket {
            request_type: 0x00,
            request: REQ_SET_CONFIGURATION,
            value: 1,
            index: 0,
            length: 0,
        });
        assert_eq!(kb.handle_in(0, &mut empty), UsbHandshake::Ack { bytes: 0 });

        // Halt interrupt endpoint 0x81.
        kb.handle_setup(SetupPacket {
            request_type: 0x02,
            request: REQ_SET_FEATURE,
            value: FEATURE_ENDPOINT_HALT,
            index: 0x81,
            length: 0,
        });
        assert_eq!(kb.handle_in(0, &mut empty), UsbHandshake::Ack { bytes: 0 });

        kb.handle_setup(SetupPacket {
            request_type: 0x82,
            request: REQ_GET_STATUS,
            value: 0,
            index: 0x81,
            length: 2,
        });
        assert_eq!(kb.handle_in(0, &mut status), UsbHandshake::Ack { bytes: 2 });
        assert_eq!(status, [0x01, 0x00]);
        assert_eq!(kb.handle_out(0, &[]), UsbHandshake::Ack { bytes: 0 });

        let mut buf = [0u8; 8];
        assert_eq!(kb.handle_in(1, &mut buf), UsbHandshake::Stall);

        // Clear halt and verify the endpoint goes back to NAK (no report queued).
        kb.handle_setup(SetupPacket {
            request_type: 0x02,
            request: REQ_CLEAR_FEATURE,
            value: FEATURE_ENDPOINT_HALT,
            index: 0x81,
            length: 0,
        });
        assert_eq!(kb.handle_in(0, &mut empty), UsbHandshake::Ack { bytes: 0 });
        assert_eq!(kb.handle_in(1, &mut buf), UsbHandshake::Nak);
    }
}
