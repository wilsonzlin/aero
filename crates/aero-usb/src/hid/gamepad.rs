use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::device::UsbInResult;
use crate::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

use super::{
    build_string_descriptor_utf16le, clamp_response, HidProtocol, HID_REQUEST_GET_IDLE,
    HID_REQUEST_GET_PROTOCOL, HID_REQUEST_GET_REPORT, HID_REQUEST_SET_IDLE,
    HID_REQUEST_SET_PROTOCOL, USB_DESCRIPTOR_TYPE_CONFIGURATION, USB_DESCRIPTOR_TYPE_DEVICE,
    USB_DESCRIPTOR_TYPE_HID, USB_DESCRIPTOR_TYPE_HID_REPORT, USB_DESCRIPTOR_TYPE_STRING,
    USB_FEATURE_DEVICE_REMOTE_WAKEUP, USB_FEATURE_ENDPOINT_HALT, USB_REQUEST_CLEAR_FEATURE,
    USB_REQUEST_GET_CONFIGURATION, USB_REQUEST_GET_DESCRIPTOR, USB_REQUEST_GET_INTERFACE,
    USB_REQUEST_GET_STATUS, USB_REQUEST_SET_ADDRESS, USB_REQUEST_SET_CONFIGURATION,
    USB_REQUEST_SET_FEATURE, USB_REQUEST_SET_INTERFACE,
};

const INTERRUPT_IN_EP: u8 = 0x81;
const MAX_PENDING_REPORTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Canonical input fields for Aero's USB HID gamepad report.
///
/// The 8-byte packed layout is kept in sync with the browser-side packing helpers
/// (`web/src/input/gamepad.ts`) via shared fixtures:
/// - `docs/fixtures/hid_gamepad_report_vectors.json` (in-range packing/layout)
/// - `docs/fixtures/hid_gamepad_report_clamping_vectors.json` (clamping/masking semantics)
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
    pub fn to_bytes(self) -> [u8; 8] {
        let [b0, b1] = self.buttons.to_le_bytes();
        [
            b0,
            b1,
            self.hat & 0x0f,
            self.x as u8,
            self.y as u8,
            self.rx as u8,
            self.ry as u8,
            0x00,
        ]
    }
}

pub(super) fn sanitize_gamepad_report_bytes(bytes: [u8; 8]) -> [u8; 8] {
    let hat = (bytes[2] & 0x0f).min(8);
    let clamp_axis = |b: u8| (b as i8).clamp(-127, 127) as u8;
    [
        bytes[0],
        bytes[1],
        hat,
        clamp_axis(bytes[3]),
        clamp_axis(bytes[4]),
        clamp_axis(bytes[5]),
        clamp_axis(bytes[6]),
        0,
    ]
}

#[derive(Debug)]
pub struct UsbHidGamepad {
    address: u8,
    configuration: u8,
    remote_wakeup_enabled: bool,
    remote_wakeup_pending: bool,
    suspended: bool,
    interrupt_in_halted: bool,
    idle_rate: u8,
    protocol: HidProtocol,

    buttons: u16,
    hat: u8,
    x: i8,
    y: i8,
    rx: i8,
    ry: i8,

    last_report: [u8; 8],
    pending_reports: VecDeque<[u8; 8]>,
}

/// Shareable handle for a USB HID gamepad model.
#[derive(Clone, Debug)]
pub struct UsbHidGamepadHandle(Rc<RefCell<UsbHidGamepad>>);

impl UsbHidGamepadHandle {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(UsbHidGamepad::new())))
    }

    pub fn configured(&self) -> bool {
        self.0.borrow().configuration != 0
    }

    /// Sets or clears a button.
    ///
    /// `button_idx` is **1-based** and maps directly to HID usages Button 1..16.
    pub fn button_event(&self, button_idx: u8, pressed: bool) {
        self.0.borrow_mut().button_event(button_idx, pressed);
    }

    pub fn set_buttons(&self, buttons: u16) {
        self.0.borrow_mut().set_buttons(buttons);
    }

    /// Sets the hat switch direction.
    ///
    /// - `None` means centered (null state).
    /// - `Some(0..=7)` corresponds to N, NE, E, SE, S, SW, W, NW.
    pub fn set_hat(&self, hat: Option<u8>) {
        self.0.borrow_mut().set_hat(hat);
    }

    pub fn set_axes(&self, x: i8, y: i8, rx: i8, ry: i8) {
        self.0.borrow_mut().set_axes(x, y, rx, ry);
    }

    /// Updates the entire 8-byte gamepad report state in one call.
    ///
    /// This is useful for host-side gamepad polling, where the full state is refreshed at a
    /// fixed rate and should enqueue at most one report per poll.
    pub fn set_report(&self, report: GamepadReport) {
        self.0.borrow_mut().set_report(report);
    }
}

impl Default for UsbHidGamepadHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbHidGamepadHandle {
    fn reset_host_state_for_restore(&mut self) {
        self.0.borrow_mut().reset_host_state_for_restore();
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

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        self.0.borrow_mut().handle_interrupt_in(ep_addr)
    }

    fn set_suspended(&mut self, suspended: bool) {
        self.0.borrow_mut().set_suspended(suspended);
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        self.0.borrow_mut().poll_remote_wakeup()
    }
}

impl Default for UsbHidGamepad {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for UsbHidGamepad {
    const DEVICE_ID: [u8; 4] = *b"UGPD";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_CONFIGURATION: u16 = 2;
        const TAG_REMOTE_WAKEUP: u16 = 3;
        const TAG_REMOTE_WAKEUP_PENDING: u16 = 4;
        const TAG_SUSPENDED: u16 = 5;
        const TAG_INTERRUPT_IN_HALTED: u16 = 6;
        const TAG_IDLE_RATE: u16 = 7;
        const TAG_PROTOCOL: u16 = 8;
        const TAG_BUTTONS: u16 = 9;
        const TAG_HAT: u16 = 10;
        const TAG_X: u16 = 11;
        const TAG_Y: u16 = 12;
        const TAG_RX: u16 = 13;
        const TAG_RY: u16 = 14;
        const TAG_LAST_REPORT: u16 = 15;
        const TAG_PENDING_REPORTS: u16 = 16;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u8(TAG_ADDRESS, self.address);
        w.field_u8(TAG_CONFIGURATION, self.configuration);
        w.field_bool(TAG_REMOTE_WAKEUP, self.remote_wakeup_enabled);
        w.field_bool(TAG_REMOTE_WAKEUP_PENDING, self.remote_wakeup_pending);
        w.field_bool(TAG_SUSPENDED, self.suspended);
        w.field_bool(TAG_INTERRUPT_IN_HALTED, self.interrupt_in_halted);
        w.field_u8(TAG_IDLE_RATE, self.idle_rate);
        w.field_u8(TAG_PROTOCOL, self.protocol as u8);

        w.field_u16(TAG_BUTTONS, self.buttons);
        w.field_u8(TAG_HAT, self.hat);
        w.field_u8(TAG_X, self.x as u8);
        w.field_u8(TAG_Y, self.y as u8);
        w.field_u8(TAG_RX, self.rx as u8);
        w.field_u8(TAG_RY, self.ry as u8);

        w.field_bytes(TAG_LAST_REPORT, self.last_report.to_vec());
        let pending: Vec<Vec<u8>> = self.pending_reports.iter().map(|r| r.to_vec()).collect();
        w.field_bytes(
            TAG_PENDING_REPORTS,
            Encoder::new().vec_bytes(&pending).finish(),
        );

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_CONFIGURATION: u16 = 2;
        const TAG_REMOTE_WAKEUP: u16 = 3;
        const TAG_REMOTE_WAKEUP_PENDING: u16 = 4;
        const TAG_SUSPENDED: u16 = 5;
        const TAG_INTERRUPT_IN_HALTED: u16 = 6;
        const TAG_IDLE_RATE: u16 = 7;
        const TAG_PROTOCOL: u16 = 8;
        const TAG_BUTTONS: u16 = 9;
        const TAG_HAT: u16 = 10;
        const TAG_X: u16 = 11;
        const TAG_Y: u16 = 12;
        const TAG_RX: u16 = 13;
        const TAG_RY: u16 = 14;
        const TAG_LAST_REPORT: u16 = 15;
        const TAG_PENDING_REPORTS: u16 = 16;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        *self = Self::new();

        let address = r.u8(TAG_ADDRESS)?.unwrap_or(0);
        self.address = if address <= 127 { address } else { 0 };
        let configuration = r.u8(TAG_CONFIGURATION)?.unwrap_or(0);
        self.configuration = if configuration == 0 { 0 } else { 1 };
        self.remote_wakeup_enabled = r.bool(TAG_REMOTE_WAKEUP)?.unwrap_or(false);
        self.remote_wakeup_pending = r.bool(TAG_REMOTE_WAKEUP_PENDING)?.unwrap_or(false);
        self.suspended = r.bool(TAG_SUSPENDED)?.unwrap_or(false);
        self.interrupt_in_halted = r.bool(TAG_INTERRUPT_IN_HALTED)?.unwrap_or(false);
        self.idle_rate = r.u8(TAG_IDLE_RATE)?.unwrap_or(0);

        if let Some(protocol) = r.u8(TAG_PROTOCOL)? {
            self.protocol = match protocol {
                0 => HidProtocol::Boot,
                1 => HidProtocol::Report,
                _ => return Err(SnapshotError::InvalidFieldEncoding("hid protocol")),
            };
        }

        self.buttons = r.u16(TAG_BUTTONS)?.unwrap_or(0);
        let hat = r.u8(TAG_HAT)?.unwrap_or(8);
        self.hat = if hat <= 8 { hat } else { 8 };
        self.x = (r.u8(TAG_X)?.unwrap_or(0) as i8).clamp(-127, 127);
        self.y = (r.u8(TAG_Y)?.unwrap_or(0) as i8).clamp(-127, 127);
        self.rx = (r.u8(TAG_RX)?.unwrap_or(0) as i8).clamp(-127, 127);
        self.ry = (r.u8(TAG_RY)?.unwrap_or(0) as i8).clamp(-127, 127);

        if let Some(buf) = r.bytes(TAG_LAST_REPORT) {
            if buf.len() != self.last_report.len() {
                return Err(SnapshotError::InvalidFieldEncoding("gamepad last report"));
            }
            let mut report = [0u8; 8];
            report.copy_from_slice(buf);
            self.last_report = sanitize_gamepad_report_bytes(report);
        }

        if let Some(buf) = r.bytes(TAG_PENDING_REPORTS) {
            let mut d = Decoder::new(buf);
            self.pending_reports.clear();
            let count = d.u32()? as usize;
            if count > MAX_PENDING_REPORTS {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "gamepad pending reports",
                ));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if len != self.last_report.len() {
                    return Err(SnapshotError::InvalidFieldEncoding("gamepad report length"));
                }
                let report = d.bytes_vec(len)?;
                let report = report.try_into().expect("len checked");
                self.pending_reports
                    .push_back(sanitize_gamepad_report_bytes(report));
            }
            d.finish()?;
        }

        Ok(())
    }
}

impl IoSnapshot for UsbHidGamepadHandle {
    const DEVICE_ID: [u8; 4] = UsbHidGamepad::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = UsbHidGamepad::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.0.borrow().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        self.0.borrow_mut().load_state(bytes)
    }
}

impl UsbHidGamepad {
    pub fn new() -> Self {
        let initial_report = GamepadReport {
            buttons: 0,
            hat: 8,
            x: 0,
            y: 0,
            rx: 0,
            ry: 0,
        }
        .to_bytes();

        Self {
            address: 0,
            configuration: 0,
            remote_wakeup_enabled: false,
            remote_wakeup_pending: false,
            suspended: false,
            interrupt_in_halted: false,
            idle_rate: 0,
            protocol: HidProtocol::Report,
            buttons: 0,
            hat: 8,
            x: 0,
            y: 0,
            rx: 0,
            ry: 0,
            last_report: initial_report,
            pending_reports: VecDeque::new(),
        }
    }

    pub fn button_event(&mut self, button_idx: u8, pressed: bool) {
        if !(1..=16).contains(&button_idx) {
            return;
        }
        let bit = 1u16 << (button_idx - 1);
        let before = self.buttons;
        if pressed {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        if before != self.buttons {
            self.enqueue_current_report();
        }
    }

    pub fn set_buttons(&mut self, buttons: u16) {
        if self.buttons != buttons {
            self.buttons = buttons;
            self.enqueue_current_report();
        }
    }

    pub fn set_hat(&mut self, hat: Option<u8>) {
        let hat = match hat {
            Some(v) if v <= 7 => v,
            _ => 8,
        };
        if self.hat != hat {
            self.hat = hat;
            self.enqueue_current_report();
        }
    }

    pub fn set_axes(&mut self, x: i8, y: i8, rx: i8, ry: i8) {
        let x = x.clamp(-127, 127);
        let y = y.clamp(-127, 127);
        let rx = rx.clamp(-127, 127);
        let ry = ry.clamp(-127, 127);

        if self.x != x || self.y != y || self.rx != rx || self.ry != ry {
            self.x = x;
            self.y = y;
            self.rx = rx;
            self.ry = ry;
            self.enqueue_current_report();
        }
    }

    pub fn set_report(&mut self, report: GamepadReport) {
        let hat = match report.hat {
            v if v <= 7 => v,
            _ => 8,
        };
        let x = report.x.clamp(-127, 127);
        let y = report.y.clamp(-127, 127);
        let rx = report.rx.clamp(-127, 127);
        let ry = report.ry.clamp(-127, 127);

        if self.buttons == report.buttons
            && self.hat == hat
            && self.x == x
            && self.y == y
            && self.rx == rx
            && self.ry == ry
        {
            return;
        }

        self.buttons = report.buttons;
        self.hat = hat;
        self.x = x;
        self.y = y;
        self.rx = rx;
        self.ry = ry;
        self.enqueue_current_report();
    }

    pub fn current_input_report(&self) -> GamepadReport {
        GamepadReport {
            buttons: self.buttons,
            hat: self.hat,
            x: self.x,
            y: self.y,
            rx: self.rx,
            ry: self.ry,
        }
    }

    fn enqueue_current_report(&mut self) {
        // USB interrupt endpoints are not active until the device has been configured. Track the
        // current state regardless, but do not buffer reports that would get delivered later as
        // stale input.
        if self.configuration == 0 {
            return;
        }

        let report = self.current_input_report().to_bytes();
        if report != self.last_report {
            self.last_report = report;
            if self.pending_reports.len() >= MAX_PENDING_REPORTS {
                self.pending_reports.pop_front();
            }
            self.pending_reports.push_back(report);
            if self.suspended && self.remote_wakeup_enabled {
                self.remote_wakeup_pending = true;
            }
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(build_string_descriptor_utf16le("Aero")),
            2 => Some(build_string_descriptor_utf16le("Aero USB HID Gamepad")),
            _ => None,
        }
    }

    fn hid_descriptor_bytes(&self) -> [u8; 9] {
        let report_len = HID_REPORT_DESCRIPTOR.len() as u16;
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
}

impl UsbDeviceModel for UsbHidGamepad {
    fn reset(&mut self) {
        *self = Self::new();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        match (setup.request_type(), setup.recipient()) {
            (RequestType::Standard, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
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
                            || setup.w_length != 0
                        {
                            return ControlResponse::Stall;
                        }
                        self.remote_wakeup_enabled = false;
                        self.remote_wakeup_pending = false;
                        ControlResponse::Ack
                    }
                    _ => ControlResponse::Stall,
                },
                USB_REQUEST_SET_FEATURE => match setup.w_value {
                    USB_FEATURE_DEVICE_REMOTE_WAKEUP => {
                        if setup.request_direction() != RequestDirection::HostToDevice
                            || setup.w_index != 0
                            || setup.w_length != 0
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
                        || setup.w_length != 0
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
                        USB_DESCRIPTOR_TYPE_DEVICE => Some(DEVICE_DESCRIPTOR.to_vec()),
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => Some(CONFIG_DESCRIPTOR.to_vec()),
                        USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                        _ => None,
                    };
                    data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                        .unwrap_or(ControlResponse::Stall)
                }
                USB_REQUEST_SET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let config = (setup.w_value & 0x00ff) as u8;
                    if config > 1 {
                        return ControlResponse::Stall;
                    }
                    let prev = self.configuration;
                    self.configuration = config;
                    if self.configuration == 0 {
                        self.pending_reports.clear();
                        self.remote_wakeup_pending = false;
                    } else if prev == 0 {
                        // We drop interrupt reports while unconfigured. When the host configures
                        // the device, enqueue a report for the current state (if non-default) so
                        // held buttons/axes become visible without requiring a new input event.
                        self.pending_reports.clear();
                        self.remote_wakeup_pending = false;
                        self.last_report = GamepadReport {
                            buttons: 0,
                            hat: 8,
                            x: 0,
                            y: 0,
                            rx: 0,
                            ry: 0,
                        }
                        .to_bytes();
                        self.enqueue_current_report();
                        // Enqueueing the held-state report above is part of the host configuration
                        // transition. Do not treat it as input activity for remote wakeup.
                        self.remote_wakeup_pending = false;
                    }
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.configuration], setup.w_length))
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Interface) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0, 0], setup.w_length))
                }
                USB_REQUEST_GET_INTERFACE => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
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
                    if setup.w_index == 0 && setup.w_value == 0 && setup.w_length == 0 {
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let desc_type = setup.descriptor_type();
                    let data = match desc_type {
                        USB_DESCRIPTOR_TYPE_HID_REPORT => Some(HID_REPORT_DESCRIPTOR.to_vec()),
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
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index != INTERRUPT_IN_EP as u16 {
                        return ControlResponse::Stall;
                    }
                    let status: u16 = if self.interrupt_in_halted { 1 } else { 0 };
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
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT
                        && setup.w_index == INTERRUPT_IN_EP as u16
                    {
                        self.interrupt_in_halted = false;
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
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT
                        && setup.w_index == INTERRUPT_IN_EP as u16
                    {
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
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let report_type = (setup.w_value >> 8) as u8;
                    match report_type {
                        1 => ControlResponse::Data(clamp_response(
                            self.current_input_report().to_bytes().to_vec(),
                            setup.w_length,
                        )),
                        _ => ControlResponse::Stall,
                    }
                }
                HID_REQUEST_GET_IDLE => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.idle_rate], setup.w_length))
                }
                HID_REQUEST_SET_IDLE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    self.idle_rate = (setup.w_value >> 8) as u8;
                    ControlResponse::Ack
                }
                HID_REQUEST_GET_PROTOCOL => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.protocol as u8], setup.w_length))
                }
                HID_REQUEST_SET_PROTOCOL => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                    {
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

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        if ep_addr != INTERRUPT_IN_EP {
            return UsbInResult::Stall;
        }
        if self.configuration == 0 {
            return UsbInResult::Nak;
        }
        if self.interrupt_in_halted {
            return UsbInResult::Stall;
        }
        match self.pending_reports.pop_front() {
            Some(r) => UsbInResult::Data(r.to_vec()),
            None => UsbInResult::Nak,
        }
    }

    fn set_suspended(&mut self, suspended: bool) {
        if self.suspended == suspended {
            return;
        }
        self.suspended = suspended;
        self.remote_wakeup_pending = false;
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        if self.remote_wakeup_pending
            && self.remote_wakeup_enabled
            && self.configuration != 0
            && self.suspended
        {
            self.remote_wakeup_pending = false;
            true
        } else {
            false
        }
    }
}

// USB device descriptor (Gamepad)
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
    34,
    0x00, // wTotalLength
    0x01, // bNumInterfaces
    0x01, // bConfigurationValue
    0x00, // iConfiguration
    0xa0, // bmAttributes (bus powered + remote wake)
    50,   // bMaxPower (100mA)
    // Interface descriptor
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    0x00, // bInterfaceNumber
    0x00, // bAlternateSetting
    0x01, // bNumEndpoints
    0x03, // bInterfaceClass (HID)
    0x00, // bInterfaceSubClass
    0x00, // bInterfaceProtocol
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
    0x03,            // bmAttributes (Interrupt)
    0x08,
    0x00, // wMaxPacketSize (8)
    0x0a, // bInterval (10ms)
];

pub(super) static HID_REPORT_DESCRIPTOR: [u8; 76] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x05, // Usage (Game Pad)
    0xa1, 0x01, // Collection (Application)
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
    0x46, 0x3b, 0x01, // Physical Maximum (315)
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
    0x25, 0x7f, // Logical Maximum (127)
    0x75, 0x08, // Report Size (8)
    0x95, 0x04, // Report Count (4)
    0x81, 0x02, // Input (Data,Var,Abs) Axes
    0x75, 0x08, // Report Size (8)
    0x95, 0x01, // Report Count (1)
    0x81, 0x01, // Input (Const,Array,Abs) Padding
    0xc0, // End Collection
];

#[cfg(test)]
mod tests {
    use super::*;

    fn w_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn configure_gamepad(pad: &mut UsbHidGamepad) {
        assert_eq!(
            pad.handle_control_request(
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
        let mut pad = UsbHidGamepad::new();
        let dev = match pad.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: USB_REQUEST_GET_DESCRIPTOR,
                w_value: (USB_DESCRIPTOR_TYPE_DEVICE as u16) << 8,
                w_index: 0,
                w_length: 18,
            },
            None,
        ) {
            ControlResponse::Data(data) => data,
            other => panic!("expected Data response, got {other:?}"),
        };
        assert_eq!(dev.len(), 18);
        assert_eq!(dev[0] as usize, dev.len());
        assert_eq!(dev[1], USB_DESCRIPTOR_TYPE_DEVICE);
    }

    #[test]
    fn config_descriptor_has_expected_layout() {
        let mut pad = UsbHidGamepad::new();
        let cfg = match pad.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: USB_REQUEST_GET_DESCRIPTOR,
                w_value: (USB_DESCRIPTOR_TYPE_CONFIGURATION as u16) << 8,
                w_index: 0,
                w_length: CONFIG_DESCRIPTOR.len() as u16,
            },
            None,
        ) {
            ControlResponse::Data(data) => data,
            other => panic!("expected Data response, got {other:?}"),
        };
        assert_eq!(cfg[0], 0x09);
        assert_eq!(cfg[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(w_le(&cfg, 2) as usize, cfg.len());
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
        assert_eq!(w_le(ep, 4), 8);
    }

    #[test]
    fn get_report_returns_current_state() {
        let mut pad = UsbHidGamepad::new();
        pad.set_buttons(0x0001);
        pad.set_hat(Some(2));
        pad.set_axes(1, -1, 5, -5);

        let resp = pad.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1,
                b_request: HID_REQUEST_GET_REPORT,
                w_value: 0x0100,
                w_index: 0,
                w_length: 8,
            },
            None,
        );

        assert_eq!(
            resp,
            ControlResponse::Data(vec![0x01, 0x00, 0x02, 0x01, 0xff, 0x05, 0xfb, 0x00])
        );
    }

    #[test]
    fn configuration_enqueues_held_state() {
        let mut pad = UsbHidGamepad::new();
        pad.button_event(1, true);
        assert_eq!(pad.handle_in_transfer(INTERRUPT_IN_EP, 8), UsbInResult::Nak);

        configure_gamepad(&mut pad);
        assert_eq!(
            pad.handle_in_transfer(INTERRUPT_IN_EP, 8),
            UsbInResult::Data(vec![0x01, 0x00, 0x08, 0, 0, 0, 0, 0])
        );
    }

    #[test]
    fn configuration_enqueues_held_state_without_triggering_remote_wakeup() {
        let mut pad = UsbHidGamepad::new();

        assert_eq!(
            pad.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00, // HostToDevice | Standard | Device
                    b_request: USB_REQUEST_SET_FEATURE,
                    w_value: USB_FEATURE_DEVICE_REMOTE_WAKEUP,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
        pad.set_suspended(true);

        pad.button_event(1, true);
        assert_eq!(pad.handle_in_transfer(INTERRUPT_IN_EP, 8), UsbInResult::Nak);

        configure_gamepad(&mut pad);
        assert!(
            !pad.poll_remote_wakeup(),
            "configuration should not surface the held-state report as a remote wakeup event"
        );
        assert_eq!(
            pad.handle_in_transfer(INTERRUPT_IN_EP, 8),
            UsbInResult::Data(vec![0x01, 0x00, 0x08, 0, 0, 0, 0, 0])
        );
    }

    #[test]
    fn configuration_does_not_replay_transient_button_click() {
        let mut pad = UsbHidGamepad::new();
        pad.button_event(1, true);
        pad.button_event(1, false);
        assert_eq!(pad.handle_in_transfer(INTERRUPT_IN_EP, 8), UsbInResult::Nak);

        configure_gamepad(&mut pad);
        assert_eq!(pad.handle_in_transfer(INTERRUPT_IN_EP, 8), UsbInResult::Nak);
    }

    #[test]
    fn report_queue_is_bounded_and_deduped() {
        let mut pad = UsbHidGamepad::new();
        configure_gamepad(&mut pad);

        pad.button_event(1, true);
        assert_eq!(pad.pending_reports.len(), 1);
        pad.enqueue_current_report();
        assert_eq!(pad.pending_reports.len(), 1);

        for _ in 0..(MAX_PENDING_REPORTS + 32) {
            pad.button_event(1, true);
            pad.button_event(1, false);
        }

        assert!(pad.pending_reports.len() <= MAX_PENDING_REPORTS);
    }

    #[test]
    fn snapshot_restore_rejects_oversized_pending_reports_count() {
        const TAG_PENDING_REPORTS: u16 = 16;

        let snapshot = {
            let mut w =
                SnapshotWriter::new(UsbHidGamepad::DEVICE_ID, UsbHidGamepad::DEVICE_VERSION);
            w.field_bytes(
                TAG_PENDING_REPORTS,
                Encoder::new().u32(MAX_PENDING_REPORTS as u32 + 1).finish(),
            );
            w.finish()
        };

        let mut pad = UsbHidGamepad::new();
        match pad.load_state(&snapshot) {
            Err(SnapshotError::InvalidFieldEncoding("gamepad pending reports")) => {}
            other => panic!("expected InvalidFieldEncoding, got {other:?}"),
        }
    }
}
