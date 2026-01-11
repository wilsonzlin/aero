use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::io::usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

use super::{
    build_string_descriptor_utf16le, clamp_response, keyboard::KeyboardReport, mouse::MouseReport,
    HidProtocol, HID_REQUEST_GET_IDLE, HID_REQUEST_GET_PROTOCOL, HID_REQUEST_GET_REPORT,
    HID_REQUEST_SET_IDLE, HID_REQUEST_SET_PROTOCOL, HID_REQUEST_SET_REPORT,
    USB_DESCRIPTOR_TYPE_CONFIGURATION, USB_DESCRIPTOR_TYPE_DEVICE, USB_DESCRIPTOR_TYPE_HID,
    USB_DESCRIPTOR_TYPE_HID_REPORT, USB_DESCRIPTOR_TYPE_STRING, USB_FEATURE_DEVICE_REMOTE_WAKEUP,
    USB_FEATURE_ENDPOINT_HALT, USB_REQUEST_CLEAR_FEATURE, USB_REQUEST_GET_CONFIGURATION,
    USB_REQUEST_GET_DESCRIPTOR, USB_REQUEST_GET_INTERFACE, USB_REQUEST_GET_STATUS,
    USB_REQUEST_SET_ADDRESS, USB_REQUEST_SET_CONFIGURATION, USB_REQUEST_SET_FEATURE,
    USB_REQUEST_SET_INTERFACE,
};

const KEYBOARD_INTERFACE: u8 = 0;
const MOUSE_INTERFACE: u8 = 1;
const GAMEPAD_INTERFACE: u8 = 2;

const KEYBOARD_INTERRUPT_IN_EP: u8 = 0x81;
const MOUSE_INTERRUPT_IN_EP: u8 = 0x82;
const GAMEPAD_INTERRUPT_IN_EP: u8 = 0x83;

const MAX_PENDING_KEYBOARD_REPORTS: usize = 64;
const MAX_PENDING_MOUSE_REPORTS: usize = 128;
const MAX_PENDING_GAMEPAD_REPORTS: usize = 64;

#[derive(Debug, Clone)]
struct KeyboardInterface {
    idle_rate: u8,
    protocol: HidProtocol,
    leds: u8,

    modifiers: u8,
    pressed_keys: Vec<u8>,

    last_report: [u8; 8],
    pending_reports: VecDeque<[u8; 8]>,
}

impl KeyboardInterface {
    fn new() -> Self {
        Self {
            idle_rate: 0,
            protocol: HidProtocol::Report,
            leds: 0,
            modifiers: 0,
            pressed_keys: Vec::new(),
            last_report: [0; 8],
            pending_reports: VecDeque::new(),
        }
    }

    fn clear_reports(&mut self) {
        self.pending_reports.clear();
    }

    fn key_event(&mut self, usage: u8, pressed: bool) {
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

    fn current_input_report(&self) -> KeyboardReport {
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
            if self.pending_reports.len() >= MAX_PENDING_KEYBOARD_REPORTS {
                self.pending_reports.pop_front();
            }
            self.pending_reports.push_back(report);
        }
    }

    fn poll_interrupt_in(&mut self) -> Option<Vec<u8>> {
        self.pending_reports.pop_front().map(|r| r.to_vec())
    }
}

#[derive(Debug, Clone)]
struct MouseInterface {
    idle_rate: u8,
    protocol: HidProtocol,

    buttons: u8,
    dx: i32,
    dy: i32,
    wheel: i32,

    pending_reports: VecDeque<MouseReport>,
}

impl MouseInterface {
    fn new() -> Self {
        Self {
            idle_rate: 0,
            protocol: HidProtocol::Report,
            buttons: 0,
            dx: 0,
            dy: 0,
            wheel: 0,
            pending_reports: VecDeque::new(),
        }
    }

    fn clear_reports(&mut self) {
        self.pending_reports.clear();
    }

    fn push_report(&mut self, report: MouseReport) {
        if self.pending_reports.len() >= MAX_PENDING_MOUSE_REPORTS {
            self.pending_reports.pop_front();
        }
        self.pending_reports.push_back(report);
    }

    fn button_event(&mut self, button_bit: u8, pressed: bool) {
        self.flush_motion();
        let before = self.buttons;
        if pressed {
            self.buttons |= button_bit;
        } else {
            self.buttons &= !button_bit;
        }
        if self.buttons != before {
            self.push_report(MouseReport {
                buttons: self.buttons,
                x: 0,
                y: 0,
                wheel: 0,
            });
        }
    }

    fn movement(&mut self, dx: i32, dy: i32) {
        self.dx += dx;
        self.dy += dy;
        self.flush_motion();
    }

    fn wheel(&mut self, delta: i32) {
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

            self.push_report(MouseReport {
                buttons: self.buttons,
                x: step_x,
                y: step_y,
                wheel: step_wheel,
            });
        }
    }

    fn poll_interrupt_in(&mut self) -> Option<Vec<u8>> {
        self.pending_reports
            .pop_front()
            .map(|r| r.to_bytes(self.protocol))
    }
}

#[derive(Debug, Clone)]
struct GamepadInterface {
    idle_rate: u8,
    protocol: HidProtocol,

    buttons: u16,
    axes: [i8; 6],

    last_report: [u8; 8],
    pending_reports: VecDeque<[u8; 8]>,
}

impl GamepadInterface {
    fn new() -> Self {
        Self {
            idle_rate: 0,
            protocol: HidProtocol::Report,
            buttons: 0,
            axes: [0; 6],
            last_report: [0; 8],
            pending_reports: VecDeque::new(),
        }
    }

    fn clear_reports(&mut self) {
        self.pending_reports.clear();
    }

    fn button_event(&mut self, button_mask: u16, pressed: bool) {
        let before = self.buttons;
        if pressed {
            self.buttons |= button_mask;
        } else {
            self.buttons &= !button_mask;
        }
        if before != self.buttons {
            self.enqueue_current_report();
        }
    }

    fn current_input_report(&self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..2].copy_from_slice(&self.buttons.to_le_bytes());
        for (idx, axis) in self.axes.iter().enumerate() {
            out[2 + idx] = *axis as u8;
        }
        out
    }

    fn enqueue_current_report(&mut self) {
        let report = self.current_input_report();
        if report != self.last_report {
            self.last_report = report;
            if self.pending_reports.len() >= MAX_PENDING_GAMEPAD_REPORTS {
                self.pending_reports.pop_front();
            }
            self.pending_reports.push_back(report);
        }
    }

    fn poll_interrupt_in(&mut self) -> Option<Vec<u8>> {
        self.pending_reports.pop_front().map(|r| r.to_vec())
    }
}

#[derive(Debug)]
pub struct UsbCompositeHidInput {
    address: u8,
    configuration: u8,
    remote_wakeup_enabled: bool,
    keyboard: KeyboardInterface,
    mouse: MouseInterface,
    gamepad: GamepadInterface,
    keyboard_interrupt_in_halted: bool,
    mouse_interrupt_in_halted: bool,
    gamepad_interrupt_in_halted: bool,
}

/// Shareable handle for a USB composite HID device (keyboard + mouse + gamepad).
///
/// This consumes a single root hub port while exposing three HID interfaces.
#[derive(Clone, Debug)]
pub struct UsbCompositeHidInputHandle(Rc<RefCell<UsbCompositeHidInput>>);

impl UsbCompositeHidInputHandle {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(UsbCompositeHidInput::new())))
    }

    pub fn configured(&self) -> bool {
        self.0.borrow().configuration != 0
    }

    pub fn key_event(&self, usage: u8, pressed: bool) {
        self.0.borrow_mut().keyboard.key_event(usage, pressed);
    }

    pub fn mouse_button_event(&self, button_bit: u8, pressed: bool) {
        self.0.borrow_mut().mouse.button_event(button_bit, pressed);
    }

    pub fn mouse_movement(&self, dx: i32, dy: i32) {
        self.0.borrow_mut().mouse.movement(dx, dy);
    }

    pub fn mouse_wheel(&self, delta: i32) {
        self.0.borrow_mut().mouse.wheel(delta);
    }

    pub fn gamepad_button_event(&self, button_mask: u16, pressed: bool) {
        self.0
            .borrow_mut()
            .gamepad
            .button_event(button_mask, pressed);
    }
}

impl Default for UsbCompositeHidInputHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbCompositeHidInputHandle {
    fn get_device_descriptor(&self) -> &[u8] {
        &DEVICE_DESCRIPTOR
    }

    fn get_config_descriptor(&self) -> &[u8] {
        &CONFIG_DESCRIPTOR
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        // Composite devices expose per-interface report descriptors; return the keyboard
        // report descriptor as a sensible default.
        &super::keyboard::HID_REPORT_DESCRIPTOR
    }

    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        self.0
            .borrow_mut()
            .handle_control_request(setup, data_stage)
    }

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        self.0.borrow_mut().poll_interrupt_in(ep)
    }
}

impl Default for UsbCompositeHidInput {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbCompositeHidInput {
    pub fn new() -> Self {
        Self {
            address: 0,
            configuration: 0,
            remote_wakeup_enabled: false,
            keyboard: KeyboardInterface::new(),
            mouse: MouseInterface::new(),
            gamepad: GamepadInterface::new(),
            keyboard_interrupt_in_halted: false,
            mouse_interrupt_in_halted: false,
            gamepad_interrupt_in_halted: false,
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(build_string_descriptor_utf16le("Aero")),
            2 => Some(build_string_descriptor_utf16le("Aero USB Composite HID")),
            _ => None,
        }
    }

    fn hid_descriptor_bytes(report_len: u16) -> [u8; 9] {
        [
            0x09,                    // bLength
            USB_DESCRIPTOR_TYPE_HID, // bDescriptorType
            0x11,
            0x01,                           // bcdHID (1.11)
            0x00,                           // bCountryCode
            0x01,                           // bNumDescriptors
            USB_DESCRIPTOR_TYPE_HID_REPORT, // bDescriptorType (Report)
            (report_len & 0x00ff) as u8,
            (report_len >> 8) as u8,
        ]
    }

    fn report_descriptor_for_interface(interface: u8) -> Option<&'static [u8]> {
        match interface {
            KEYBOARD_INTERFACE => Some(&super::keyboard::HID_REPORT_DESCRIPTOR),
            MOUSE_INTERFACE => Some(&super::mouse::HID_REPORT_DESCRIPTOR),
            GAMEPAD_INTERFACE => Some(&GAMEPAD_REPORT_DESCRIPTOR),
            _ => None,
        }
    }

    fn hid_descriptor_for_interface(interface: u8) -> Option<[u8; 9]> {
        Self::report_descriptor_for_interface(interface).map(|report| {
            let len = report.len() as u16;
            Self::hid_descriptor_bytes(len)
        })
    }

    fn clear_reports(&mut self) {
        self.keyboard.clear_reports();
        self.mouse.clear_reports();
        self.gamepad.clear_reports();
    }

    fn interrupt_halted(&self, ep: u8) -> Option<bool> {
        match ep {
            KEYBOARD_INTERRUPT_IN_EP => Some(self.keyboard_interrupt_in_halted),
            MOUSE_INTERRUPT_IN_EP => Some(self.mouse_interrupt_in_halted),
            GAMEPAD_INTERRUPT_IN_EP => Some(self.gamepad_interrupt_in_halted),
            _ => None,
        }
    }

    fn set_interrupt_halted(&mut self, ep: u8, halted: bool) -> bool {
        match ep {
            KEYBOARD_INTERRUPT_IN_EP => {
                self.keyboard_interrupt_in_halted = halted;
                true
            }
            MOUSE_INTERRUPT_IN_EP => {
                self.mouse_interrupt_in_halted = halted;
                true
            }
            GAMEPAD_INTERRUPT_IN_EP => {
                self.gamepad_interrupt_in_halted = halted;
                true
            }
            _ => false,
        }
    }
}

impl UsbDeviceModel for UsbCompositeHidInput {
    fn get_device_descriptor(&self) -> &[u8] {
        &DEVICE_DESCRIPTOR
    }

    fn get_config_descriptor(&self) -> &[u8] {
        &CONFIG_DESCRIPTOR
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        // The composite device exposes multiple report descriptors; callers should use
        // GET_DESCRIPTOR(REPORT) routed by interface number. Return the keyboard report
        // descriptor as a sane default.
        &super::keyboard::HID_REPORT_DESCRIPTOR
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        match (setup.request_type(), setup.recipient()) {
            (RequestType::Standard, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let mut status: u16 = 0;
                    if self.remote_wakeup_enabled {
                        status |= 1 << 1;
                    }
                    ControlResponse::Data(clamp_response(
                        status.to_le_bytes().to_vec(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_CLEAR_FEATURE => match setup.w_value {
                    USB_FEATURE_DEVICE_REMOTE_WAKEUP => {
                        if setup.request_direction() != RequestDirection::HostToDevice
                            || setup.w_index != 0
                        {
                            return ControlResponse::Stall;
                        }
                        self.remote_wakeup_enabled = false;
                        ControlResponse::Ack
                    }
                    _ => ControlResponse::Stall,
                },
                USB_REQUEST_SET_FEATURE => match setup.w_value {
                    USB_FEATURE_DEVICE_REMOTE_WAKEUP => {
                        if setup.request_direction() != RequestDirection::HostToDevice
                            || setup.w_index != 0
                        {
                            return ControlResponse::Stall;
                        }
                        self.remote_wakeup_enabled = true;
                        ControlResponse::Ack
                    }
                    _ => ControlResponse::Stall,
                },
                USB_REQUEST_SET_ADDRESS => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                    {
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
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => {
                            Some(self.get_config_descriptor().to_vec())
                        }
                        USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                        _ => None,
                    };
                    data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                        .unwrap_or(ControlResponse::Stall)
                }
                USB_REQUEST_SET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let config = (setup.w_value & 0x00ff) as u8;
                    if config > 1 {
                        return ControlResponse::Stall;
                    }
                    self.configuration = config;
                    if self.configuration == 0 {
                        self.clear_reports();
                    }
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.configuration], setup.w_length))
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Interface) => {
                let interface = (setup.w_index & 0x00ff) as u8;
                match setup.b_request {
                    USB_REQUEST_GET_STATUS => {
                        if setup.request_direction() != RequestDirection::DeviceToHost {
                            return ControlResponse::Stall;
                        }
                        if !matches!(interface, KEYBOARD_INTERFACE | MOUSE_INTERFACE | GAMEPAD_INTERFACE)
                        {
                            return ControlResponse::Stall;
                        }
                        ControlResponse::Data(clamp_response(vec![0, 0], setup.w_length))
                    }
                    USB_REQUEST_GET_INTERFACE => {
                        if setup.request_direction() != RequestDirection::DeviceToHost {
                            return ControlResponse::Stall;
                        }
                        if matches!(interface, KEYBOARD_INTERFACE | MOUSE_INTERFACE | GAMEPAD_INTERFACE)
                        {
                            ControlResponse::Data(clamp_response(vec![0], setup.w_length))
                        } else {
                            ControlResponse::Stall
                        }
                    }
                    USB_REQUEST_SET_INTERFACE => {
                        if setup.request_direction() != RequestDirection::HostToDevice {
                            return ControlResponse::Stall;
                        }
                        if matches!(interface, KEYBOARD_INTERFACE | MOUSE_INTERFACE | GAMEPAD_INTERFACE)
                            && setup.w_value == 0
                        {
                            ControlResponse::Ack
                        } else {
                            ControlResponse::Stall
                        }
                    }
                    USB_REQUEST_GET_DESCRIPTOR => {
                        if setup.request_direction() != RequestDirection::DeviceToHost {
                            return ControlResponse::Stall;
                        }
                        let desc_type = setup.descriptor_type();
                        let data = match desc_type {
                            USB_DESCRIPTOR_TYPE_HID_REPORT => {
                                Self::report_descriptor_for_interface(interface)
                                    .map(|d| d.to_vec())
                            }
                            USB_DESCRIPTOR_TYPE_HID => Self::hid_descriptor_for_interface(interface)
                                .map(|d| d.to_vec()),
                            _ => None,
                        };
                        data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                            .unwrap_or(ControlResponse::Stall)
                    }
                    _ => ControlResponse::Stall,
                }
            }
            (RequestType::Standard, RequestRecipient::Endpoint) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    let Some(halted) = self.interrupt_halted(ep) else {
                        return ControlResponse::Stall;
                    };
                    let status: u16 = if halted { 1 } else { 0 };
                    ControlResponse::Data(clamp_response(
                        status.to_le_bytes().to_vec(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT && self.set_interrupt_halted(ep, false)
                    {
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT && self.set_interrupt_halted(ep, true)
                    {
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Interface) => {
                let interface = (setup.w_index & 0x00ff) as u8;
                match interface {
                    KEYBOARD_INTERFACE => match setup.b_request {
                        HID_REQUEST_GET_REPORT => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            let report_type = (setup.w_value >> 8) as u8;
                            match report_type {
                                1 => ControlResponse::Data(clamp_response(
                                    self.keyboard.current_input_report().to_bytes().to_vec(),
                                    setup.w_length,
                                )),
                                2 => ControlResponse::Data(clamp_response(
                                    vec![self.keyboard.leds],
                                    setup.w_length,
                                )),
                                _ => ControlResponse::Stall,
                            }
                        }
                        HID_REQUEST_SET_REPORT => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            let report_type = (setup.w_value >> 8) as u8;
                            match (report_type, data_stage) {
                                (2, Some(data)) if !data.is_empty() => {
                                    self.keyboard.leds = data[0];
                                    ControlResponse::Ack
                                }
                                _ => ControlResponse::Stall,
                            }
                        }
                        HID_REQUEST_GET_IDLE => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.keyboard.idle_rate],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_IDLE => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            self.keyboard.idle_rate = (setup.w_value >> 8) as u8;
                            ControlResponse::Ack
                        }
                        HID_REQUEST_GET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.keyboard.protocol as u8],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            if let Some(proto) = HidProtocol::from_u16(setup.w_value) {
                                self.keyboard.protocol = proto;
                                ControlResponse::Ack
                            } else {
                                ControlResponse::Stall
                            }
                        }
                        _ => ControlResponse::Stall,
                    },
                    MOUSE_INTERFACE => match setup.b_request {
                        HID_REQUEST_GET_REPORT => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            let report = MouseReport {
                                buttons: self.mouse.buttons,
                                x: 0,
                                y: 0,
                                wheel: 0,
                            }
                            .to_bytes(self.mouse.protocol);
                            ControlResponse::Data(clamp_response(report, setup.w_length))
                        }
                        HID_REQUEST_GET_IDLE => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.mouse.idle_rate],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_IDLE => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            self.mouse.idle_rate = (setup.w_value >> 8) as u8;
                            ControlResponse::Ack
                        }
                        HID_REQUEST_GET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.mouse.protocol as u8],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            if let Some(proto) = HidProtocol::from_u16(setup.w_value) {
                                self.mouse.protocol = proto;
                                ControlResponse::Ack
                            } else {
                                ControlResponse::Stall
                            }
                        }
                        _ => ControlResponse::Stall,
                    },
                    GAMEPAD_INTERFACE => match setup.b_request {
                        HID_REQUEST_GET_REPORT => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                self.gamepad.current_input_report().to_vec(),
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_GET_IDLE => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.gamepad.idle_rate],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_IDLE => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            self.gamepad.idle_rate = (setup.w_value >> 8) as u8;
                            ControlResponse::Ack
                        }
                        HID_REQUEST_GET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.gamepad.protocol as u8],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            if let Some(proto) = HidProtocol::from_u16(setup.w_value) {
                                self.gamepad.protocol = proto;
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
            _ => ControlResponse::Stall,
        }
    }

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        if self.configuration == 0 {
            return None;
        }

        match ep {
            KEYBOARD_INTERRUPT_IN_EP => {
                if self.keyboard_interrupt_in_halted {
                    return None;
                }
                self.keyboard.poll_interrupt_in()
            }
            MOUSE_INTERRUPT_IN_EP => {
                if self.mouse_interrupt_in_halted {
                    return None;
                }
                self.mouse.poll_interrupt_in()
            }
            GAMEPAD_INTERRUPT_IN_EP => {
                if self.gamepad_interrupt_in_halted {
                    return None;
                }
                self.gamepad.poll_interrupt_in()
            }
            _ => None,
        }
    }
}

fn modifier_bit(usage: u8) -> Option<u8> {
    (0xe0..=0xe7)
        .contains(&usage)
        .then(|| 1u8 << (usage - 0xe0))
}

// USB device descriptor (Composite HID: keyboard + mouse + gamepad).
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
    0x03,
    0x00, // idProduct (0x0003)
    0x01,
    0x00, // bcdDevice (1.00)
    0x01, // iManufacturer
    0x02, // iProduct
    0x00, // iSerialNumber
    0x01, // bNumConfigurations
];

// USB configuration descriptor tree:
//   Config(9) + 3 * (Interface(9) + HID(9) + Endpoint(7)) = 84 bytes
static CONFIG_DESCRIPTOR: [u8; 84] = [
    // Configuration descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_CONFIGURATION,
    84,
    0x00, // wTotalLength
    0x03, // bNumInterfaces
    0x01, // bConfigurationValue
    0x00, // iConfiguration
    0xa0, // bmAttributes (bus powered + remote wake)
    50,   // bMaxPower (100mA)
    // Interface 0: Keyboard
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    KEYBOARD_INTERFACE, // bInterfaceNumber
    0x00,               // bAlternateSetting
    0x01,               // bNumEndpoints
    0x03,               // bInterfaceClass (HID)
    0x01,               // bInterfaceSubClass (Boot)
    0x01,               // bInterfaceProtocol (Keyboard)
    0x00,               // iInterface
    // HID descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_HID,
    0x11,
    0x01, // bcdHID (1.11)
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    USB_DESCRIPTOR_TYPE_HID_REPORT,
    super::keyboard::HID_REPORT_DESCRIPTOR.len() as u8,
    0x00, // wDescriptorLength
    // Endpoint descriptor (Interrupt IN)
    0x07, // bLength
    super::USB_DESCRIPTOR_TYPE_ENDPOINT,
    KEYBOARD_INTERRUPT_IN_EP, // bEndpointAddress
    0x03,                     // bmAttributes (Interrupt)
    0x08,
    0x00, // wMaxPacketSize (8)
    0x0a, // bInterval (10ms)
    // Interface 1: Mouse
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    MOUSE_INTERFACE, // bInterfaceNumber
    0x00,            // bAlternateSetting
    0x01,            // bNumEndpoints
    0x03,            // bInterfaceClass (HID)
    0x01,            // bInterfaceSubClass (Boot)
    0x02,            // bInterfaceProtocol (Mouse)
    0x00,            // iInterface
    // HID descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_HID,
    0x11,
    0x01, // bcdHID (1.11)
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    USB_DESCRIPTOR_TYPE_HID_REPORT,
    super::mouse::HID_REPORT_DESCRIPTOR.len() as u8,
    0x00, // wDescriptorLength
    // Endpoint descriptor (Interrupt IN)
    0x07, // bLength
    super::USB_DESCRIPTOR_TYPE_ENDPOINT,
    MOUSE_INTERRUPT_IN_EP, // bEndpointAddress
    0x03,                  // bmAttributes (Interrupt)
    0x04,
    0x00, // wMaxPacketSize (4)
    0x0a, // bInterval (10ms)
    // Interface 2: Gamepad
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    GAMEPAD_INTERFACE, // bInterfaceNumber
    0x00,              // bAlternateSetting
    0x01,              // bNumEndpoints
    0x03,              // bInterfaceClass (HID)
    0x00,              // bInterfaceSubClass
    0x00,              // bInterfaceProtocol
    0x00,              // iInterface
    // HID descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_HID,
    0x11,
    0x01, // bcdHID (1.11)
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    USB_DESCRIPTOR_TYPE_HID_REPORT,
    GAMEPAD_REPORT_DESCRIPTOR.len() as u8,
    0x00, // wDescriptorLength
    // Endpoint descriptor (Interrupt IN)
    0x07, // bLength
    super::USB_DESCRIPTOR_TYPE_ENDPOINT,
    GAMEPAD_INTERRUPT_IN_EP, // bEndpointAddress
    0x03,                    // bmAttributes (Interrupt)
    0x08,
    0x00, // wMaxPacketSize (8)
    0x0a, // bInterval (10ms)
];

// Simple gamepad: 16 buttons (2 bytes) + 6 signed axes (6 bytes) = 8-byte report.
static GAMEPAD_REPORT_DESCRIPTOR: [u8; 47] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x05, // Usage (Game Pad)
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x10, //   Report Count (16)
    0x05, 0x09, //   Usage Page (Button)
    0x19, 0x01, //   Usage Minimum (Button 1)
    0x29, 0x10, //   Usage Maximum (Button 16)
    0x81, 0x02, //   Input (Data,Var,Abs)
    0x05, 0x01, //   Usage Page (Generic Desktop)
    0x15, 0x81, //   Logical Minimum (-127)
    0x25, 0x7f, //   Logical Maximum (127)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x06, //   Report Count (6)
    0x09, 0x30, //   Usage (X)
    0x09, 0x31, //   Usage (Y)
    0x09, 0x32, //   Usage (Z)
    0x09, 0x35, //   Usage (Rz)
    0x09, 0x33, //   Usage (Rx)
    0x09, 0x34, //   Usage (Ry)
    0x81, 0x02, //   Input (Data,Var,Abs)
    0xc0, // End Collection
];

#[cfg(test)]
mod tests {
    use super::*;

    fn w_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    #[test]
    fn config_descriptor_has_three_interfaces_and_endpoints() {
        let dev = UsbCompositeHidInput::new();
        let cfg = dev.get_config_descriptor();
        assert_eq!(cfg[0], 0x09);
        assert_eq!(cfg[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(w_le(cfg, 2) as usize, cfg.len());
        assert_eq!(cfg[4], 3);

        let mut ifaces = 0;
        let mut eps = Vec::new();
        let mut off = 9usize;
        while off + 2 <= cfg.len() {
            let len = cfg[off] as usize;
            let dtype = cfg[off + 1];
            match dtype {
                super::super::USB_DESCRIPTOR_TYPE_INTERFACE => ifaces += 1,
                super::super::USB_DESCRIPTOR_TYPE_ENDPOINT => eps.push(cfg[off + 2]),
                _ => {}
            }
            off += len;
            if len == 0 {
                break;
            }
        }

        assert_eq!(ifaces, 3);
        assert_eq!(eps.len(), 3);
        eps.sort_unstable();
        eps.dedup();
        assert_eq!(eps, vec![0x81, 0x82, 0x83]);
    }

    #[test]
    fn hid_descriptors_reference_correct_report_lengths() {
        let dev = UsbCompositeHidInput::new();
        let cfg = dev.get_config_descriptor();

        // Layout is deterministic: HID descriptors start after each 9-byte interface descriptor.
        // Interface0 HID at 18, Interface1 HID at 43, Interface2 HID at 68.
        let hid0 = &cfg[18..27];
        let hid1 = &cfg[43..52];
        let hid2 = &cfg[68..77];

        assert_eq!(hid0[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(
            w_le(hid0, 7) as usize,
            super::super::keyboard::HID_REPORT_DESCRIPTOR.len()
        );

        assert_eq!(hid1[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(
            w_le(hid1, 7) as usize,
            super::super::mouse::HID_REPORT_DESCRIPTOR.len()
        );

        assert_eq!(hid2[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(w_le(hid2, 7) as usize, GAMEPAD_REPORT_DESCRIPTOR.len());
    }

    #[test]
    fn get_descriptor_report_dispatches_by_interface_number() {
        let mut dev = UsbCompositeHidInput::new();

        for (iface, expected) in [
            (KEYBOARD_INTERFACE, &super::super::keyboard::HID_REPORT_DESCRIPTOR[..]),
            (MOUSE_INTERFACE, &super::super::mouse::HID_REPORT_DESCRIPTOR[..]),
            (GAMEPAD_INTERFACE, &GAMEPAD_REPORT_DESCRIPTOR[..]),
        ] {
            let resp = dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x81,
                    b_request: USB_REQUEST_GET_DESCRIPTOR,
                    w_value: (USB_DESCRIPTOR_TYPE_HID_REPORT as u16) << 8,
                    w_index: iface as u16,
                    w_length: expected.len() as u16,
                },
                None,
            );
            assert_eq!(resp, ControlResponse::Data(expected.to_vec()));
        }
    }
}
