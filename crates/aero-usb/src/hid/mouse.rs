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
    USB_DESCRIPTOR_TYPE_ENDPOINT, USB_DESCRIPTOR_TYPE_HID, USB_DESCRIPTOR_TYPE_HID_REPORT,
    USB_DESCRIPTOR_TYPE_INTERFACE, USB_DESCRIPTOR_TYPE_STRING, USB_FEATURE_DEVICE_REMOTE_WAKEUP,
    USB_FEATURE_ENDPOINT_HALT, USB_REQUEST_CLEAR_FEATURE, USB_REQUEST_GET_CONFIGURATION,
    USB_REQUEST_GET_DESCRIPTOR, USB_REQUEST_GET_INTERFACE, USB_REQUEST_GET_STATUS,
    USB_REQUEST_SET_ADDRESS, USB_REQUEST_SET_CONFIGURATION, USB_REQUEST_SET_FEATURE,
    USB_REQUEST_SET_INTERFACE,
};

const INTERRUPT_IN_EP: u8 = 0x81;
const MAX_PENDING_REPORTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    pub buttons: u8,
    pub x: i8,
    pub y: i8,
    pub wheel: i8,
    pub hwheel: i8,
}

impl MouseReport {
    pub fn to_bytes(self, protocol: HidProtocol) -> Vec<u8> {
        let clamp_axis = |v: i8| v.clamp(-127, 127) as u8;
        match protocol {
            // Boot mouse protocol is fixed-format and only defines 3 buttons.
            HidProtocol::Boot => vec![self.buttons & 0x07, clamp_axis(self.x), clamp_axis(self.y)],
            // Report protocol uses our full report descriptor, which supports 5 buttons + wheel +
            // horizontal wheel (AC Pan).
            HidProtocol::Report => vec![
                self.buttons & 0x1f,
                clamp_axis(self.x),
                clamp_axis(self.y),
                clamp_axis(self.wheel),
                clamp_axis(self.hwheel),
            ],
        }
    }
}

#[derive(Debug)]
pub struct UsbHidMouse {
    address: u8,
    configuration: u8,
    remote_wakeup_enabled: bool,
    remote_wakeup_pending: bool,
    suspended: bool,
    interrupt_in_halted: bool,
    idle_rate: u8,
    protocol: HidProtocol,

    ticks_ms: u32,
    last_interrupt_in_ms: u32,

    buttons: u8,
    dx: i32,
    dy: i32,
    wheel: i32,
    hwheel: i32,

    pending_reports: VecDeque<MouseReport>,
}

/// Shareable handle for a USB HID mouse model.
#[derive(Clone, Debug)]
pub struct UsbHidMouseHandle(Rc<RefCell<UsbHidMouse>>);

impl UsbHidMouseHandle {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(UsbHidMouse::new())))
    }

    pub fn configured(&self) -> bool {
        self.0.borrow().configuration != 0
    }

    pub fn button_event(&self, button_bit: u8, pressed: bool) {
        self.0.borrow_mut().button_event(button_bit, pressed);
    }

    pub fn movement(&self, dx: i32, dy: i32) {
        self.0.borrow_mut().movement(dx, dy);
    }

    pub fn wheel(&self, delta: i32) {
        self.0.borrow_mut().wheel(delta);
    }

    pub fn hwheel(&self, delta: i32) {
        self.0.borrow_mut().hwheel(delta);
    }

    /// Inject a vertical + horizontal wheel update (AC Pan) into the mouse report stream.
    ///
    /// This allows callers to emit a single report that contains both axes, matching how physical
    /// pointing devices often report diagonal scrolling.
    pub fn wheel2(&self, wheel: i32, hwheel: i32) {
        self.0.borrow_mut().wheel2(wheel, hwheel);
    }
}

impl Default for UsbHidMouseHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbHidMouseHandle {
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

    fn tick_1ms(&mut self) {
        self.0.borrow_mut().tick_1ms();
    }

    fn set_suspended(&mut self, suspended: bool) {
        self.0.borrow_mut().set_suspended(suspended);
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        self.0.borrow_mut().poll_remote_wakeup()
    }
}

impl Default for UsbHidMouse {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for UsbHidMouse {
    const DEVICE_ID: [u8; 4] = *b"UMSE";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 2);

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
        const TAG_DX: u16 = 10;
        const TAG_DY: u16 = 11;
        const TAG_WHEEL: u16 = 12;
        const TAG_PENDING_REPORTS: u16 = 13;
        const TAG_TICKS_MS: u16 = 14;
        const TAG_LAST_INTERRUPT_IN_MS: u16 = 15;
        const TAG_HWHEEL: u16 = 16;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u8(TAG_ADDRESS, self.address);
        w.field_u8(TAG_CONFIGURATION, self.configuration);
        w.field_bool(TAG_REMOTE_WAKEUP, self.remote_wakeup_enabled);
        w.field_bool(TAG_REMOTE_WAKEUP_PENDING, self.remote_wakeup_pending);
        w.field_bool(TAG_SUSPENDED, self.suspended);
        w.field_bool(TAG_INTERRUPT_IN_HALTED, self.interrupt_in_halted);
        w.field_u8(TAG_IDLE_RATE, self.idle_rate);
        w.field_u8(TAG_PROTOCOL, self.protocol as u8);
        w.field_u8(TAG_BUTTONS, self.buttons);
        w.field_u32(TAG_TICKS_MS, self.ticks_ms);
        w.field_u32(TAG_LAST_INTERRUPT_IN_MS, self.last_interrupt_in_ms);
        w.field_i32(TAG_DX, self.dx);
        w.field_i32(TAG_DY, self.dy);
        w.field_i32(TAG_WHEEL, self.wheel);
        w.field_i32(TAG_HWHEEL, self.hwheel);

        let pending: Vec<Vec<u8>> = self
            .pending_reports
            .iter()
            .map(|r| {
                vec![
                    r.buttons,
                    r.x as u8,
                    r.y as u8,
                    r.wheel as u8,
                    r.hwheel as u8,
                ]
            })
            .collect();
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
        const TAG_DX: u16 = 10;
        const TAG_DY: u16 = 11;
        const TAG_WHEEL: u16 = 12;
        const TAG_PENDING_REPORTS: u16 = 13;
        const TAG_TICKS_MS: u16 = 14;
        const TAG_LAST_INTERRUPT_IN_MS: u16 = 15;
        const TAG_HWHEEL: u16 = 16;

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

        // Button state is a 5-bit mask (left/right/middle/back/forward). Clamp to avoid carrying
        // arbitrary padding bits from untrusted snapshots into subsequent state transitions.
        self.buttons = r.u8(TAG_BUTTONS)?.unwrap_or(0) & 0x1f;
        self.ticks_ms = r.u32(TAG_TICKS_MS)?.unwrap_or(0);
        self.last_interrupt_in_ms = r.u32(TAG_LAST_INTERRUPT_IN_MS)?.unwrap_or(0);
        self.dx = r.i32(TAG_DX)?.unwrap_or(0);
        self.dy = r.i32(TAG_DY)?.unwrap_or(0);
        self.wheel = r.i32(TAG_WHEEL)?.unwrap_or(0);
        self.hwheel = r.i32(TAG_HWHEEL)?.unwrap_or(0);

        if let Some(buf) = r.bytes(TAG_PENDING_REPORTS) {
            let mut d = Decoder::new(buf);
            self.pending_reports.clear();
            let count = d.u32()? as usize;
            if count > MAX_PENDING_REPORTS {
                return Err(SnapshotError::InvalidFieldEncoding("mouse pending reports"));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                // Version 1.1 stored 4-byte mouse reports (buttons, dx, dy, wheel).
                // Version 1.2 adds a 5th byte for the horizontal wheel (AC Pan).
                if len != 4 && len != 5 {
                    return Err(SnapshotError::InvalidFieldEncoding("mouse report length"));
                }
                let report = d.bytes(len)?;
                self.pending_reports.push_back(MouseReport {
                    buttons: report[0] & 0x1f,
                    x: report[1] as i8,
                    y: report[2] as i8,
                    wheel: report[3] as i8,
                    hwheel: report.get(4).copied().unwrap_or(0) as i8,
                });
            }
            d.finish()?;
        }

        Ok(())
    }
}

impl IoSnapshot for UsbHidMouseHandle {
    const DEVICE_ID: [u8; 4] = UsbHidMouse::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = UsbHidMouse::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.0.borrow().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        self.0.borrow_mut().load_state(bytes)
    }
}

impl UsbHidMouse {
    pub fn new() -> Self {
        Self {
            address: 0,
            configuration: 0,
            remote_wakeup_enabled: false,
            remote_wakeup_pending: false,
            suspended: false,
            interrupt_in_halted: false,
            idle_rate: 0,
            protocol: HidProtocol::Report,
            ticks_ms: 0,
            last_interrupt_in_ms: 0,
            buttons: 0,
            dx: 0,
            dy: 0,
            wheel: 0,
            hwheel: 0,
            pending_reports: VecDeque::new(),
        }
    }

    fn push_report(&mut self, report: MouseReport) {
        // USB interrupt endpoints are not active until the device is configured. We still track
        // button state, but do not buffer motion/button reports that would get replayed later as
        // stale input.
        if self.configuration == 0 {
            return;
        }
        if self.pending_reports.len() >= MAX_PENDING_REPORTS {
            self.pending_reports.pop_front();
        }
        self.pending_reports.push_back(report);
        if self.suspended && self.remote_wakeup_enabled {
            self.remote_wakeup_pending = true;
        }
    }

    /// Sets or clears a mouse button bit.
    ///
    /// Bit 0 = left, bit 1 = right, bit 2 = middle, bit 3 = side/back, bit 4 = extra/forward.
    pub fn button_event(&mut self, button_bit: u8, pressed: bool) {
        let bit = button_bit & 0x1f;
        if bit == 0 {
            return;
        }
        self.flush_motion();
        // Ignore any stale padding bits (e.g. from a corrupt snapshot) when determining whether the
        // guest-visible button state actually changed.
        let visible_mask = match self.protocol {
            HidProtocol::Boot => 0x07,
            HidProtocol::Report => 0x1f,
        };
        let before_full = self.buttons & 0x1f;
        let before_visible = before_full & visible_mask;
        if pressed {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        self.buttons &= 0x1f;
        let after_full = self.buttons & 0x1f;
        let after_visible = after_full & visible_mask;
        if after_visible != before_visible {
            self.push_report(MouseReport {
                buttons: self.buttons,
                x: 0,
                y: 0,
                wheel: 0,
                hwheel: 0,
            });
        } else if after_full != before_full && self.suspended && self.remote_wakeup_enabled {
            // Even though boot protocol cannot represent button4/5, treat them as user activity for
            // remote wakeup so the device behaves like physical mice that can resume a suspended
            // host from any button press.
            self.remote_wakeup_pending = true;
        }
    }

    pub fn movement(&mut self, dx: i32, dy: i32) {
        // Host input is untrusted; use saturating math so extreme values cannot overflow before we
        // clamp/split them into HID reports.
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        self.flush_motion();
    }

    pub fn wheel(&mut self, delta: i32) {
        self.wheel2(delta, 0);
    }

    pub fn hwheel(&mut self, delta: i32) {
        self.wheel2(0, delta);
    }

    pub fn wheel2(&mut self, wheel: i32, hwheel: i32) {
        self.wheel = self.wheel.saturating_add(wheel);
        self.hwheel = self.hwheel.saturating_add(hwheel);
        self.flush_motion();
    }

    fn flush_motion(&mut self) {
        // USB interrupt endpoints are inactive until the guest sets a configuration. Align with
        // `push_report` semantics by dropping any accumulated motion immediately rather than
        // looping to "split" a delta into reports that will be discarded anyway.
        if self.configuration == 0 {
            self.dx = 0;
            self.dy = 0;
            self.wheel = 0;
            self.hwheel = 0;
            return;
        }

        // HID boot protocol mice have no wheel/hwheel fields. Avoid spamming the pending report
        // queue with no-op packets when a host injects scroll events while the guest has the mouse
        // in boot mode. Still treat scroll as user activity for remote-wakeup purposes.
        if self.protocol == HidProtocol::Boot {
            if (self.wheel != 0 || self.hwheel != 0) && self.suspended && self.remote_wakeup_enabled
            {
                self.remote_wakeup_pending = true;
            }
            self.wheel = 0;
            self.hwheel = 0;
        }

        // Host input is untrusted and can be arbitrarily large. Don't allow a single injected
        // delta to spin in an unbounded loop trying to produce millions of reports; cap per-flush
        // work to the size of the pending report queue and drop any remainder.
        let mut emitted = 0usize;
        while self.dx != 0 || self.dy != 0 || self.wheel != 0 || self.hwheel != 0 {
            if emitted >= MAX_PENDING_REPORTS {
                self.dx = 0;
                self.dy = 0;
                self.wheel = 0;
                self.hwheel = 0;
                break;
            }
            let step_x = self.dx.clamp(-127, 127) as i8;
            let step_y = self.dy.clamp(-127, 127) as i8;
            let step_wheel = self.wheel.clamp(-127, 127) as i8;
            let step_hwheel = self.hwheel.clamp(-127, 127) as i8;

            self.dx -= step_x as i32;
            self.dy -= step_y as i32;
            self.wheel -= step_wheel as i32;
            self.hwheel -= step_hwheel as i32;

            self.push_report(MouseReport {
                buttons: self.buttons,
                x: step_x,
                y: step_y,
                wheel: step_wheel,
                hwheel: step_hwheel,
            });
            emitted += 1;
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(build_string_descriptor_utf16le("Aero")),
            2 => Some(build_string_descriptor_utf16le("Aero USB HID Mouse")),
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

impl UsbDeviceModel for UsbHidMouse {
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
                        self.dx = 0;
                        self.dy = 0;
                        self.wheel = 0;
                        self.hwheel = 0;
                    } else if prev == 0 {
                        // We drop motion/button reports while unconfigured. When the host
                        // configures the device, enqueue the current button state (if any) so held
                        // buttons are visible without requiring another button transition.
                        self.pending_reports.clear();
                        self.remote_wakeup_pending = false;
                        self.dx = 0;
                        self.dy = 0;
                        self.wheel = 0;
                        self.hwheel = 0;
                        let visible_mask = match self.protocol {
                            HidProtocol::Boot => 0x07,
                            HidProtocol::Report => 0x1f,
                        };
                        if (self.buttons & visible_mask) != 0 {
                            self.push_report(MouseReport {
                                buttons: self.buttons,
                                x: 0,
                                y: 0,
                                wheel: 0,
                                hwheel: 0,
                            });
                        }
                        // Enqueueing the held-button report above is a host-driven configuration
                        // transition, not a user-driven wake event; do not surface it as remote
                        // wakeup activity.
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
                    let report = MouseReport {
                        buttons: self.buttons,
                        x: 0,
                        y: 0,
                        wheel: 0,
                        hwheel: 0,
                    }
                    .to_bytes(self.protocol);
                    ControlResponse::Data(clamp_response(report, setup.w_length))
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
                    // HID 1.11 7.2.4: SET_IDLE loads the idle timer; restart our last-report
                    // timestamp.
                    self.last_interrupt_in_ms = self.ticks_ms;
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
        if let Some(r) = self.pending_reports.pop_front() {
            self.last_interrupt_in_ms = self.ticks_ms;
            return UsbInResult::Data(r.to_bytes(self.protocol));
        }

        if self.idle_rate != 0 {
            let idle_ms = u32::from(self.idle_rate) * 4;
            if self.ticks_ms.wrapping_sub(self.last_interrupt_in_ms) >= idle_ms {
                self.last_interrupt_in_ms = self.ticks_ms;
                let report = MouseReport {
                    buttons: self.buttons,
                    x: 0,
                    y: 0,
                    wheel: 0,
                    hwheel: 0,
                }
                .to_bytes(self.protocol);
                return UsbInResult::Data(report);
            }
        }

        UsbInResult::Nak
    }

    fn tick_1ms(&mut self) {
        self.ticks_ms = self.ticks_ms.wrapping_add(1);
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

// USB device descriptor (Mouse)
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
    0x02,
    0x00, // idProduct (0x0002)
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
    USB_DESCRIPTOR_TYPE_INTERFACE,
    0x00, // bInterfaceNumber
    0x00, // bAlternateSetting
    0x01, // bNumEndpoints
    0x03, // bInterfaceClass (HID)
    0x01, // bInterfaceSubClass (Boot)
    0x02, // bInterfaceProtocol (Mouse)
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
    USB_DESCRIPTOR_TYPE_ENDPOINT,
    INTERRUPT_IN_EP, // bEndpointAddress
    0x03,            // bmAttributes (Interrupt)
    0x05,
    0x00, // wMaxPacketSize (5)
    0x0a, // bInterval (10ms)
];

pub(super) static HID_REPORT_DESCRIPTOR: [u8; 61] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x01, // Usage (Pointer)
    0xa1, 0x00, // Collection (Physical)
    0x05, 0x09, // Usage Page (Buttons)
    0x19, 0x01, // Usage Minimum (Button 1)
    0x29, 0x05, // Usage Maximum (Button 5)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x01, // Logical Maximum (1)
    0x95, 0x05, // Report Count (5)
    0x75, 0x01, // Report Size (1)
    0x81, 0x02, // Input (Data,Var,Abs) Button bits
    0x95, 0x01, // Report Count (1)
    0x75, 0x03, // Report Size (3)
    0x81, 0x01, // Input (Const,Array,Abs) Padding
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x30, // Usage (X)
    0x09, 0x31, // Usage (Y)
    0x09, 0x38, // Usage (Wheel)
    0x15, 0x81, // Logical Minimum (-127)
    0x25, 0x7f, // Logical Maximum (127)
    0x75, 0x08, // Report Size (8)
    0x95, 0x03, // Report Count (3)
    0x81, 0x06, // Input (Data,Var,Rel) X,Y,Wheel
    0x05, 0x0c, // Usage Page (Consumer)
    0x0a, 0x38, 0x02, // Usage (AC Pan)
    0x95, 0x01, // Report Count (1)
    0x81, 0x06, // Input (Data,Var,Rel) AC Pan (horizontal wheel)
    0xc0, // End Collection
    0xc0, // End Collection
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn w_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn poll_interrupt_in(dev: &mut UsbHidMouse) -> Option<Vec<u8>> {
        match dev.handle_in_transfer(INTERRUPT_IN_EP, 5) {
            UsbInResult::Data(data) => Some(data),
            UsbInResult::Nak => None,
            UsbInResult::Stall => panic!("unexpected STALL on interrupt IN"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT on interrupt IN"),
        }
    }

    fn configure_mouse(mouse: &mut UsbHidMouse) {
        assert_eq!(
            mouse.handle_control_request(
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
    fn mouse_descriptors_reference_report_length() {
        let mut mouse = UsbHidMouse::new();
        let cfg = match mouse.handle_control_request(
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
        assert_eq!(cfg[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(w_le(&cfg, 2) as usize, cfg.len());

        let hid = &cfg[18..27];
        assert_eq!(hid[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(w_le(hid, 7) as usize, HID_REPORT_DESCRIPTOR.len());
    }

    #[test]
    fn report_descriptor_supports_five_buttons_in_report_protocol() {
        // The synthetic USB mouse models 5 buttons (left/right/middle/back/forward) in report
        // protocol. Ensure the report descriptor declares Button UsageMax=5 and ReportCount=5 so
        // those bits are not treated as padding by HID parsers.
        assert!(
            HID_REPORT_DESCRIPTOR
                .windows(4)
                .any(|w| w == [0x19, 0x01, 0x29, 0x05]),
            "expected mouse descriptor to declare Button usage range 1..=5"
        );
        assert!(
            HID_REPORT_DESCRIPTOR.windows(2).any(|w| w == [0x95, 0x05]),
            "expected mouse descriptor to use ReportCount=5 for button bits"
        );
        assert!(
            HID_REPORT_DESCRIPTOR.windows(2).any(|w| w == [0x75, 0x03]),
            "expected mouse descriptor to use 3 bits of padding after the 5 button bits"
        );
    }

    #[test]
    fn mouse_motion_splits_large_deltas() {
        let mut mouse = UsbHidMouse::new();
        configure_mouse(&mut mouse);
        mouse.movement(200, 0);

        let r1 = poll_interrupt_in(&mut mouse).unwrap();
        assert_eq!(r1, vec![0x00, 127u8, 0u8, 0u8, 0u8]);

        let r2 = poll_interrupt_in(&mut mouse).unwrap();
        assert_eq!(r2, vec![0x00, 73u8, 0u8, 0u8, 0u8]);
    }

    #[test]
    fn motion_saturates_accumulator_without_overflow() {
        let mut mouse = UsbHidMouse::new();
        configure_mouse(&mut mouse);

        // Simulate a hostile/corrupt snapshot restore that leaves a huge accumulated delta in the
        // device state. Adding any further motion must not overflow and panic in debug builds.
        mouse.dx = i32::MAX;
        mouse.movement(1, 0);

        assert_eq!(mouse.pending_reports.len(), MAX_PENDING_REPORTS);

        let mut first = None;
        let mut last = None;
        let mut count = 0usize;
        while let Some(report) = poll_interrupt_in(&mut mouse) {
            if first.is_none() {
                first = Some(report.clone());
            }
            last = Some(report);
            count += 1;
        }

        assert_eq!(count, MAX_PENDING_REPORTS);
        assert_eq!(first.unwrap(), vec![0x00, 127u8, 0u8, 0u8, 0u8]);
        assert_eq!(last.unwrap(), vec![0x00, 127u8, 0u8, 0u8, 0u8]);
    }

    #[test]
    fn mouse_button_event_generates_report() {
        let mut mouse = UsbHidMouse::new();
        configure_mouse(&mut mouse);
        mouse.button_event(0x01, true);
        let r = poll_interrupt_in(&mut mouse).unwrap();
        assert_eq!(r, vec![0x01, 0, 0, 0, 0]);
    }

    #[test]
    fn mouse_standard_requests_accept_set_address_and_remote_wakeup() {
        let mut mouse = UsbHidMouse::new();

        assert_eq!(
            mouse.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00,
                    b_request: USB_REQUEST_SET_ADDRESS,
                    w_value: 9,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
        assert_eq!(mouse.address, 9);

        assert_eq!(
            mouse.handle_control_request(
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

        let resp = mouse.handle_control_request(
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
    }

    #[test]
    fn stalls_on_wrong_direction() {
        let mut mouse = UsbHidMouse::new();
        let resp = mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00,
                b_request: USB_REQUEST_GET_DESCRIPTOR,
                w_value: (USB_DESCRIPTOR_TYPE_DEVICE as u16) << 8,
                w_index: 0,
                w_length: 18,
            },
            None,
        );
        assert_eq!(resp, ControlResponse::Stall);
    }

    #[test]
    fn does_not_buffer_motion_reports_while_unconfigured() {
        let mut mouse = UsbHidMouse::new();
        mouse.movement(10, 0);
        assert!(poll_interrupt_in(&mut mouse).is_none());

        configure_mouse(&mut mouse);
        assert!(poll_interrupt_in(&mut mouse).is_none());
    }

    #[test]
    fn configuration_enqueues_held_button_state() {
        let mut mouse = UsbHidMouse::new();

        mouse.button_event(0x01, true); // left button
        assert!(poll_interrupt_in(&mut mouse).is_none());

        configure_mouse(&mut mouse);
        let report = poll_interrupt_in(&mut mouse).expect("expected report for held button");
        assert_eq!(report, vec![0x01, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn configuration_does_not_replay_transient_button_click() {
        let mut mouse = UsbHidMouse::new();

        mouse.button_event(0x01, true);
        mouse.button_event(0x01, false);
        assert!(poll_interrupt_in(&mut mouse).is_none());

        configure_mouse(&mut mouse);
        assert!(poll_interrupt_in(&mut mouse).is_none());
    }

    #[test]
    fn report_queue_is_bounded() {
        let mut mouse = UsbHidMouse::new();
        configure_mouse(&mut mouse);

        for _ in 0..(MAX_PENDING_REPORTS + 64) {
            mouse.movement(1, 0);
        }

        assert!(mouse.pending_reports.len() <= MAX_PENDING_REPORTS);
    }

    #[test]
    fn snapshot_restore_rejects_oversized_pending_reports_count() {
        const TAG_PENDING_REPORTS: u16 = 13;

        let snapshot = {
            let mut w = SnapshotWriter::new(UsbHidMouse::DEVICE_ID, UsbHidMouse::DEVICE_VERSION);
            w.field_bytes(
                TAG_PENDING_REPORTS,
                Encoder::new().u32(MAX_PENDING_REPORTS as u32 + 1).finish(),
            );
            w.finish()
        };

        let mut mouse = UsbHidMouse::new();
        match mouse.load_state(&snapshot) {
            Err(SnapshotError::InvalidFieldEncoding("mouse pending reports")) => {}
            other => panic!("expected InvalidFieldEncoding, got {other:?}"),
        }
    }
}
