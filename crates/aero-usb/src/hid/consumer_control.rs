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
const MAX_PENDING_REPORTS: usize = 64;
const MAX_PRESSED_USAGES: usize = 256;
const MAX_CONSUMER_USAGE: u16 = 0x03ff;

fn sanitize_consumer_usage(usage: u16) -> u16 {
    if usage <= MAX_CONSUMER_USAGE {
        usage
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsumerControlReport {
    /// Current consumer usage (0 = none pressed).
    pub usage: u16,
}

impl ConsumerControlReport {
    pub fn to_bytes(self) -> [u8; 2] {
        sanitize_consumer_usage(self.usage).to_le_bytes()
    }
}

#[derive(Debug)]
pub struct UsbHidConsumerControl {
    address: u8,
    configuration: u8,
    remote_wakeup_enabled: bool,
    remote_wakeup_pending: bool,
    suspended: bool,
    interrupt_in_halted: bool,
    idle_rate: u8,
    protocol: HidProtocol,

    pressed_usages: Vec<u16>,

    last_report: [u8; 2],
    pending_reports: VecDeque<[u8; 2]>,
}

/// Shareable handle for a USB HID Consumer Control model.
#[derive(Clone, Debug)]
pub struct UsbHidConsumerControlHandle(Rc<RefCell<UsbHidConsumerControl>>);

impl UsbHidConsumerControlHandle {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(UsbHidConsumerControl::new())))
    }

    pub fn configured(&self) -> bool {
        self.0.borrow().configuration != 0
    }

    pub fn consumer_event(&self, usage: u16, pressed: bool) {
        self.0.borrow_mut().consumer_event(usage, pressed);
    }
}

impl Default for UsbHidConsumerControlHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbHidConsumerControlHandle {
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

impl Default for UsbHidConsumerControl {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for UsbHidConsumerControl {
    const DEVICE_ID: [u8; 4] = *b"UCON";
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
        const TAG_PRESSED_USAGES: u16 = 9;
        const TAG_LAST_REPORT: u16 = 10;
        const TAG_PENDING_REPORTS: u16 = 11;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u8(TAG_ADDRESS, self.address);
        w.field_u8(TAG_CONFIGURATION, self.configuration);
        w.field_bool(TAG_REMOTE_WAKEUP, self.remote_wakeup_enabled);
        w.field_bool(TAG_REMOTE_WAKEUP_PENDING, self.remote_wakeup_pending);
        w.field_bool(TAG_SUSPENDED, self.suspended);
        w.field_bool(TAG_INTERRUPT_IN_HALTED, self.interrupt_in_halted);
        w.field_u8(TAG_IDLE_RATE, self.idle_rate);
        w.field_u8(TAG_PROTOCOL, self.protocol as u8);

        // Encode pressed usages deterministically as: u32 count + `count` little-endian u16 values.
        let mut pressed = Encoder::new().u32(self.pressed_usages.len() as u32);
        for &usage in &self.pressed_usages {
            pressed = pressed.u16(usage);
        }
        w.field_bytes(TAG_PRESSED_USAGES, pressed.finish());
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
        const TAG_PRESSED_USAGES: u16 = 9;
        const TAG_LAST_REPORT: u16 = 10;
        const TAG_PENDING_REPORTS: u16 = 11;

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

        if let Some(buf) = r.bytes(TAG_PRESSED_USAGES) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_PRESSED_USAGES {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "consumer pressed usages",
                ));
            }
            self.pressed_usages.clear();
            self.pressed_usages
                .try_reserve_exact(count)
                .map_err(|_| SnapshotError::OutOfMemory)?;
            for _ in 0..count {
                let usage = d.u16()?;
                if usage != 0 && usage <= MAX_CONSUMER_USAGE {
                    self.pressed_usages.push(usage);
                }
            }
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_LAST_REPORT) {
            if buf.len() != self.last_report.len() {
                return Err(SnapshotError::InvalidFieldEncoding("consumer last report"));
            }
            let usage = u16::from_le_bytes([buf[0], buf[1]]);
            self.last_report = sanitize_consumer_usage(usage).to_le_bytes();
        }

        if let Some(buf) = r.bytes(TAG_PENDING_REPORTS) {
            let mut d = Decoder::new(buf);
            self.pending_reports.clear();
            let count = d.u32()? as usize;
            if count > MAX_PENDING_REPORTS {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "consumer pending reports",
                ));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if len != self.last_report.len() {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "consumer report length",
                    ));
                }
                let report = d.bytes_vec(len)?;
                let report: [u8; 2] = report.try_into().expect("len checked");
                let usage = u16::from_le_bytes(report);
                self.pending_reports
                    .push_back(sanitize_consumer_usage(usage).to_le_bytes());
            }
            d.finish()?;
        }

        Ok(())
    }
}

impl IoSnapshot for UsbHidConsumerControlHandle {
    const DEVICE_ID: [u8; 4] = UsbHidConsumerControl::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = UsbHidConsumerControl::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.0.borrow().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        self.0.borrow_mut().load_state(bytes)
    }
}

impl UsbHidConsumerControl {
    pub fn new() -> Self {
        let report = ConsumerControlReport { usage: 0 }.to_bytes();
        Self {
            address: 0,
            configuration: 0,
            remote_wakeup_enabled: false,
            remote_wakeup_pending: false,
            suspended: false,
            interrupt_in_halted: false,
            idle_rate: 0,
            protocol: HidProtocol::Report,

            pressed_usages: Vec::new(),

            last_report: report,
            pending_reports: VecDeque::new(),
        }
    }

    pub fn configured(&self) -> bool {
        self.configuration != 0
    }

    pub fn consumer_event(&mut self, usage: u16, pressed: bool) {
        if usage == 0 {
            return;
        }
        if pressed {
            if usage > MAX_CONSUMER_USAGE {
                return;
            }
            if !self.pressed_usages.contains(&usage) {
                // Host input is untrusted; bound the pressed-usage stack so a stream of unique
                // (possibly bogus) consumer usages can't grow memory without limit.
                //
                // Keep the most recently pressed usages since the current report always reflects
                // the last pressed usage.
                if self.pressed_usages.len() >= MAX_PRESSED_USAGES {
                    self.pressed_usages.remove(0);
                }
                self.pressed_usages.push(usage);
            }
        } else {
            self.pressed_usages.retain(|&u| u != usage);
        }
        self.pressed_usages
            .retain(|&u| u != 0 && u <= MAX_CONSUMER_USAGE);
        self.enqueue_current_report();
    }

    fn current_input_report(&self) -> ConsumerControlReport {
        ConsumerControlReport {
            usage: sanitize_consumer_usage(self.pressed_usages.last().copied().unwrap_or(0)),
        }
    }

    fn enqueue_current_report(&mut self) {
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
            2 => Some(build_string_descriptor_utf16le("Aero USB Consumer Control")),
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

impl UsbDeviceModel for UsbHidConsumerControl {
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
                        // We drop interrupt reports while unconfigured. When the host configures the
                        // device, enqueue a report for the current state (if non-default) so a held
                        // consumer key becomes visible without requiring a new input event.
                        self.pending_reports.clear();
                        self.remote_wakeup_pending = false;
                        self.last_report = ConsumerControlReport { usage: 0 }.to_bytes();
                        self.enqueue_current_report();
                        // Enqueueing the held-state report above is a host configuration transition,
                        // not a user-triggered wake event.
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

// USB device descriptor (Consumer Control)
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
    0x04,
    0x00, // idProduct (0x0004)
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
    USB_DESCRIPTOR_TYPE_ENDPOINT,
    INTERRUPT_IN_EP, // bEndpointAddress
    0x03,            // bmAttributes (Interrupt)
    0x02,
    0x00, // wMaxPacketSize (2)
    0x0a, // bInterval (10ms)
];

pub(super) static HID_REPORT_DESCRIPTOR: [u8; 23] = [
    0x05, 0x0c, // Usage Page (Consumer)
    0x09, 0x01, // Usage (Consumer Control)
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, // Logical Minimum (0)
    0x26, 0xff, 0x03, // Logical Maximum (0x03FF)
    0x19, 0x00, // Usage Minimum (0)
    0x2a, 0xff, 0x03, // Usage Maximum (0x03FF)
    0x75, 0x10, // Report Size (16)
    0x95, 0x01, // Report Count (1)
    0x81, 0x00, // Input (Data,Array,Abs)
    0xc0, // End Collection
];

#[cfg(test)]
mod tests {
    use super::*;

    fn configure(dev: &mut UsbHidConsumerControl) {
        assert_eq!(
            dev.handle_control_request(
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
    fn pressed_usages_is_bounded() {
        let mut dev = UsbHidConsumerControl::new();

        // Press more unique usages than the bound; we should drop the oldest entries so the "last
        // pressed" ordering semantics remain intact.
        for usage in 1u16..=((MAX_PRESSED_USAGES as u16) + 10) {
            dev.consumer_event(usage, true);
        }

        assert_eq!(dev.pressed_usages.len(), MAX_PRESSED_USAGES);
        assert_eq!(dev.pressed_usages.first().copied(), Some(11));
        assert_eq!(
            dev.pressed_usages.last().copied(),
            Some((MAX_PRESSED_USAGES as u16) + 10)
        );
    }

    #[test]
    fn configuration_enqueues_held_usage_without_triggering_remote_wakeup() {
        let mut dev = UsbHidConsumerControl::new();

        assert_eq!(
            dev.handle_control_request(
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
        dev.set_suspended(true);

        dev.consumer_event(0x00e9, true); // VolumeUp
        assert_eq!(dev.handle_in_transfer(INTERRUPT_IN_EP, 2), UsbInResult::Nak);

        configure(&mut dev);
        assert!(
            !dev.poll_remote_wakeup(),
            "configuration should not surface the held-state report as a remote wakeup event"
        );
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 2),
            UsbInResult::Data(vec![0xe9, 0x00])
        );
    }

    #[test]
    fn snapshot_restore_rejects_oversized_pressed_usages_count() {
        const TAG_PRESSED_USAGES: u16 = 9;

        let snapshot = {
            let mut w = SnapshotWriter::new(
                UsbHidConsumerControl::DEVICE_ID,
                UsbHidConsumerControl::DEVICE_VERSION,
            );
            w.field_bytes(
                TAG_PRESSED_USAGES,
                Encoder::new().u32(MAX_PRESSED_USAGES as u32 + 1).finish(),
            );
            w.finish()
        };

        let mut dev = UsbHidConsumerControl::new();
        match dev.load_state(&snapshot) {
            Err(SnapshotError::InvalidFieldEncoding("consumer pressed usages")) => {}
            other => panic!("expected InvalidFieldEncoding, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_restore_rejects_oversized_pending_reports_count() {
        const TAG_PENDING_REPORTS: u16 = 11;

        let snapshot = {
            let mut w = SnapshotWriter::new(
                UsbHidConsumerControl::DEVICE_ID,
                UsbHidConsumerControl::DEVICE_VERSION,
            );
            w.field_bytes(
                TAG_PENDING_REPORTS,
                Encoder::new().u32(MAX_PENDING_REPORTS as u32 + 1).finish(),
            );
            w.finish()
        };

        let mut dev = UsbHidConsumerControl::new();
        match dev.load_state(&snapshot) {
            Err(SnapshotError::InvalidFieldEncoding("consumer pending reports")) => {}
            other => panic!("expected InvalidFieldEncoding, got {other:?}"),
        }
    }
}
