use core::any::Any;
use std::collections::{BTreeMap, VecDeque};

use crate::usb::{SetupPacket, UsbDevice, UsbHandshake, UsbSpeed};

use super::report_descriptor;

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
const DESC_INTERFACE: u8 = 0x04;
const DESC_ENDPOINT: u8 = 0x05;
const DESC_HID: u8 = 0x21;
const DESC_REPORT: u8 = 0x22;

const INTERRUPT_IN_EP_ADDR: u8 = 0x81;
const INTERRUPT_OUT_EP_ADDR: u8 = 0x01;
const INTERRUPT_EP_NUM: u8 = 1;

const DEFAULT_MAX_PACKET_SIZE0: u8 = 64;
const DEFAULT_MAX_PACKET_SIZE: u16 = 64;
const DEFAULT_MAX_PENDING_INPUT_REPORTS: usize = 256;
const DEFAULT_MAX_PENDING_OUTPUT_REPORTS: usize = 256;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbHidPassthroughOutputReport {
    /// HID report type as used by GET_REPORT/SET_REPORT:
    /// 2 = Output, 3 = Feature.
    pub report_type: u8,
    pub report_id: u8,
    /// Report payload (without the report ID prefix).
    pub data: Vec<u8>,
}

/// Generic USB HID device model with bounded report queues.
///
/// This is designed for "real device" passthrough via WebHID: the browser main thread forwards
/// `inputreport` events into [`UsbHidPassthrough::push_input_report`], and the guest can send
/// Output/Feature reports via either `SET_REPORT` control requests or interrupt OUT transfers.
#[derive(Debug)]
pub struct UsbHidPassthrough {
    address: u8,
    pending_address: Option<u8>,
    configuration: u8,
    pending_configuration: Option<u8>,
    remote_wakeup_enabled: bool,
    interrupt_in_halted: bool,
    interrupt_out_halted: bool,
    protocol: u8,
    idle_rate: u8,
    ep0: Ep0Control,

    device_descriptor: Vec<u8>,
    config_descriptor: Vec<u8>,
    hid_descriptor: Vec<u8>,
    hid_report_descriptor: Vec<u8>,
    manufacturer_string_descriptor: Vec<u8>,
    product_string_descriptor: Vec<u8>,
    serial_string_descriptor: Option<Vec<u8>>,

    has_interrupt_out: bool,
    report_ids_in_use: bool,
    input_report_lengths: BTreeMap<u8, usize>,
    output_report_lengths: BTreeMap<u8, usize>,
    feature_report_lengths: BTreeMap<u8, usize>,
    max_pending_input_reports: usize,
    max_pending_output_reports: usize,

    pending_input_reports: VecDeque<Vec<u8>>,
    last_input_reports: BTreeMap<u8, Vec<u8>>,
    last_output_reports: BTreeMap<u8, Vec<u8>>,
    last_feature_reports: BTreeMap<u8, Vec<u8>>,
    pending_output_reports: VecDeque<UsbHidPassthroughOutputReport>,
}

impl UsbHidPassthrough {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vendor_id: u16,
        product_id: u16,
        manufacturer: String,
        product: String,
        serial: Option<String>,
        hid_report_descriptor: Vec<u8>,
        has_interrupt_out: bool,
        max_packet_size: Option<u16>,
        interface_subclass: Option<u8>,
        interface_protocol: Option<u8>,
    ) -> Self {
        let max_packet_size = sanitize_max_packet_size(max_packet_size.unwrap_or(DEFAULT_MAX_PACKET_SIZE));

        let manufacturer_string_descriptor = string_descriptor_utf16le(&manufacturer);
        let product_string_descriptor = string_descriptor_utf16le(&product);
        let serial_string_descriptor = serial.as_deref().map(string_descriptor_utf16le);

        let i_serial = if serial_string_descriptor.is_some() { 3 } else { 0 };

        let device_descriptor = build_device_descriptor(
            vendor_id,
            product_id,
            DEFAULT_MAX_PACKET_SIZE0,
            1,
            2,
            i_serial,
        );

        let hid_descriptor = build_hid_descriptor(&hid_report_descriptor);
        let config_descriptor = build_config_descriptor(
            &hid_descriptor,
            has_interrupt_out,
            max_packet_size,
            interface_subclass.unwrap_or(0),
            interface_protocol.unwrap_or(0),
        );

        let (report_ids_in_use, input_report_lengths, output_report_lengths, feature_report_lengths) =
            report_descriptor_report_lengths(&hid_report_descriptor);

        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            remote_wakeup_enabled: false,
            interrupt_in_halted: false,
            interrupt_out_halted: false,
            protocol: 1, // report protocol
            idle_rate: 0,
            ep0: Ep0Control::new(),
            device_descriptor,
            config_descriptor,
            hid_descriptor,
            hid_report_descriptor,
            manufacturer_string_descriptor,
            product_string_descriptor,
            serial_string_descriptor,
            has_interrupt_out,
            report_ids_in_use,
            input_report_lengths,
            output_report_lengths,
            feature_report_lengths,
            max_pending_input_reports: DEFAULT_MAX_PENDING_INPUT_REPORTS,
            max_pending_output_reports: DEFAULT_MAX_PENDING_OUTPUT_REPORTS,
            pending_input_reports: VecDeque::new(),
            last_input_reports: BTreeMap::new(),
            last_output_reports: BTreeMap::new(),
            last_feature_reports: BTreeMap::new(),
            pending_output_reports: VecDeque::new(),
        }
    }

    pub fn configured(&self) -> bool {
        self.configuration != 0
    }

    pub fn push_input_report(&mut self, report_id: u8, data: &[u8]) {
        let mut out = Vec::with_capacity(data.len().saturating_add((report_id != 0) as usize));
        if report_id != 0 {
            out.push(report_id);
        }
        out.extend_from_slice(data);

        self.last_input_reports.insert(report_id, out.clone());

        if self.pending_input_reports.len() >= self.max_pending_input_reports {
            self.pending_input_reports.pop_front();
        }
        self.pending_input_reports.push_back(out);
    }

    pub fn pop_output_report(&mut self) -> Option<UsbHidPassthroughOutputReport> {
        self.pending_output_reports.pop_front()
    }

    fn push_output_report(&mut self, report: UsbHidPassthroughOutputReport) {
        if self.pending_output_reports.len() >= self.max_pending_output_reports {
            self.pending_output_reports.pop_front();
        }
        match report.report_type {
            2 => {
                self.last_output_reports.insert(
                    report.report_id,
                    bytes_with_report_id(report.report_id, &report.data),
                );
            }
            3 => {
                self.last_feature_reports.insert(
                    report.report_id,
                    bytes_with_report_id(report.report_id, &report.data),
                );
            }
            _ => {}
        }
        self.pending_output_reports.push_back(report);
    }

    fn report_length(&self, report_type: u8, report_id: u8) -> Option<usize> {
        match report_type {
            1 => self.input_report_lengths.get(&report_id).copied(),
            2 => self.output_report_lengths.get(&report_id).copied(),
            3 => self.feature_report_lengths.get(&report_id).copied(),
            _ => None,
        }
    }

    fn default_report(&self, report_type: u8, report_id: u8, w_length: u16) -> Vec<u8> {
        let requested = w_length as usize;
        let expected = self.report_length(report_type, report_id).unwrap_or(requested);
        let len = expected.min(requested);
        if len == 0 {
            return Vec::new();
        }

        let mut data = vec![0u8; len];
        if report_id != 0 && !data.is_empty() {
            data[0] = report_id;
        }
        data
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
            if self.configuration == 0 {
                self.pending_input_reports.clear();
                self.pending_output_reports.clear();
                self.last_input_reports.clear();
                self.last_output_reports.clear();
                self.last_feature_reports.clear();
            }
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(string_descriptor_langid(0x0409).to_vec()), // en-US
            1 => Some(self.manufacturer_string_descriptor.clone()),
            2 => Some(self.product_string_descriptor.clone()),
            3 => self.serial_string_descriptor.clone(),
            _ => None,
        }
    }

    fn get_descriptor(&self, desc_type: u8, index: u8) -> Option<Vec<u8>> {
        match desc_type {
            DESC_DEVICE => Some(self.device_descriptor.clone()),
            DESC_CONFIGURATION => Some(self.config_descriptor.clone()),
            DESC_STRING => self.string_descriptor(index).or_else(|| Some(vec![0, DESC_STRING])),
            DESC_HID => Some(self.hid_descriptor.clone()),
            DESC_REPORT => Some(self.hid_report_descriptor.clone()),
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
                let halted = if setup.index == u16::from(INTERRUPT_IN_EP_ADDR) {
                    Some(self.interrupt_in_halted)
                } else if setup.index == u16::from(INTERRUPT_OUT_EP_ADDR) && self.has_interrupt_out {
                    Some(self.interrupt_out_halted)
                } else {
                    None
                }?;

                let status: u16 = if halted { 1 } else { 0 };
                Some(status.to_le_bytes().to_vec())
            }
            (0x81, REQ_GET_INTERFACE) => ((setup.index & 0xFF) == 0).then_some(vec![0u8]),
            (0xA1, REQ_HID_GET_REPORT) => {
                let report_type = (setup.value >> 8) as u8;
                let report_id = (setup.value & 0xFF) as u8;
                let data = match report_type {
                    1 => self
                        .last_input_reports
                        .get(&report_id)
                        .cloned()
                        .unwrap_or_else(|| self.default_report(report_type, report_id, setup.length)),
                    2 => self
                        .last_output_reports
                        .get(&report_id)
                        .cloned()
                        .unwrap_or_else(|| self.default_report(report_type, report_id, setup.length)),
                    3 => self
                        .last_feature_reports
                        .get(&report_id)
                        .cloned()
                        .unwrap_or_else(|| self.default_report(report_type, report_id, setup.length)),
                    _ => return None,
                };
                Some(data)
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
                if setup.value != FEATURE_ENDPOINT_HALT {
                    return false;
                }
                if setup.index == u16::from(INTERRUPT_IN_EP_ADDR) {
                    self.interrupt_in_halted = false;
                    return true;
                }
                if setup.index == u16::from(INTERRUPT_OUT_EP_ADDR) && self.has_interrupt_out {
                    self.interrupt_out_halted = false;
                    return true;
                }
                false
            }
            (0x02, REQ_SET_FEATURE) => {
                if setup.value != FEATURE_ENDPOINT_HALT {
                    return false;
                }
                if setup.index == u16::from(INTERRUPT_IN_EP_ADDR) {
                    self.interrupt_in_halted = true;
                    return true;
                }
                if setup.index == u16::from(INTERRUPT_OUT_EP_ADDR) && self.has_interrupt_out {
                    self.interrupt_out_halted = true;
                    return true;
                }
                false
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

impl Default for UsbHidPassthrough {
    fn default() -> Self {
        Self::new(
            0x1234,
            0x5678,
            "Aero".to_string(),
            "Aero USB HID Passthrough".to_string(),
            None,
            Vec::new(),
            false,
            None,
            None,
            None,
        )
    }
}

impl UsbDevice for UsbHidPassthrough {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
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
        self.interrupt_out_halted = false;
        self.protocol = 1;
        self.idle_rate = 0;
        self.ep0 = Ep0Control::new();
        self.pending_input_reports.clear();
        self.pending_output_reports.clear();
        self.last_input_reports.clear();
        self.last_output_reports.clear();
        self.last_feature_reports.clear();
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
            // OUT requests with data stage: support SET_REPORT for Output/Feature reports.
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
        if ep == INTERRUPT_EP_NUM {
            if !self.has_interrupt_out {
                return UsbHandshake::Stall;
            }
            if self.configuration == 0 || self.interrupt_out_halted {
                return UsbHandshake::Stall;
            }

            let (report_id, payload) = if self.report_ids_in_use {
                if data.is_empty() {
                    (0, Vec::new())
                } else {
                    (data[0], data[1..].to_vec())
                }
            } else {
                (0, data.to_vec())
            };

            self.push_output_report(UsbHidPassthroughOutputReport {
                report_type: 2, // Output
                report_id,
                data: payload,
            });

            return UsbHandshake::Ack { bytes: data.len() };
        }

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
                            let report_type = (setup.value >> 8) as u8;
                            let report_id = (setup.value & 0xFF) as u8;
                            if report_type == 2 || report_type == 3 {
                                let payload = if report_id != 0 {
                                    self.report_length(report_type, report_id)
                                        .filter(|&expected_len| expected_len == self.ep0.out_data.len())
                                        .and_then(|_| self.ep0.out_data.first().copied())
                                        .filter(|&first| first == report_id)
                                        .map(|_| self.ep0.out_data[1..].to_vec())
                                        .unwrap_or_else(|| self.ep0.out_data.clone())
                                } else {
                                    self.ep0.out_data.clone()
                                };
                                self.push_output_report(UsbHidPassthroughOutputReport {
                                    report_type,
                                    report_id,
                                    data: payload,
                                });
                            }
                        }
                        _ => {}
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
        if ep == INTERRUPT_EP_NUM {
            if self.configuration == 0 {
                return UsbHandshake::Nak;
            }
            if self.interrupt_in_halted {
                return UsbHandshake::Stall;
            }
            let Some(report) = self.pending_input_reports.pop_front() else {
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

fn sanitize_max_packet_size(max_packet_size: u16) -> u16 {
    match max_packet_size {
        8 | 16 | 32 | 64 => max_packet_size,
        _ => DEFAULT_MAX_PACKET_SIZE,
    }
}

fn bytes_with_report_id(report_id: u8, payload: &[u8]) -> Vec<u8> {
    if report_id == 0 {
        return payload.to_vec();
    }
    let mut out = Vec::with_capacity(payload.len().saturating_add(1));
    out.push(report_id);
    out.extend_from_slice(payload);
    out
}

fn build_device_descriptor(
    vendor_id: u16,
    product_id: u16,
    max_packet_size0: u8,
    i_manufacturer: u8,
    i_product: u8,
    i_serial: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(18);
    out.extend_from_slice(&[
        0x12, // bLength
        DESC_DEVICE,
        0x00,
        0x02, // bcdUSB (2.00)
        0x00, // bDeviceClass (per interface)
        0x00, // bDeviceSubClass
        0x00, // bDeviceProtocol
        max_packet_size0, // bMaxPacketSize0
    ]);
    out.extend_from_slice(&vendor_id.to_le_bytes());
    out.extend_from_slice(&product_id.to_le_bytes());
    out.extend_from_slice(&0x0100u16.to_le_bytes()); // bcdDevice (1.00)
    out.push(i_manufacturer);
    out.push(i_product);
    out.push(i_serial);
    out.push(0x01); // bNumConfigurations
    debug_assert_eq!(out.len(), 18);
    out
}

fn build_hid_descriptor(report_descriptor: &[u8]) -> Vec<u8> {
    let report_len = report_descriptor.len() as u16;
    let mut out = Vec::with_capacity(9);
    out.extend_from_slice(&[
        0x09, // bLength
        DESC_HID,
        0x11,
        0x01, // bcdHID (1.11)
        0x00, // bCountryCode
        0x01, // bNumDescriptors
        DESC_REPORT,
    ]);
    out.extend_from_slice(&report_len.to_le_bytes());
    debug_assert_eq!(out.len(), 9);
    out
}

fn build_config_descriptor(
    hid_descriptor: &[u8],
    has_interrupt_out: bool,
    max_packet_size: u16,
    interface_subclass: u8,
    interface_protocol: u8,
) -> Vec<u8> {
    // Config(9) + Interface(9) + HID(9) + Endpoint IN(7) + Endpoint OUT(7 optional)
    let total_len =
        9u16 + 9u16 + hid_descriptor.len() as u16 + 7u16 + if has_interrupt_out { 7 } else { 0 };
    let num_endpoints = if has_interrupt_out { 2 } else { 1 };

    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&[
        0x09, // bLength
        DESC_CONFIGURATION,
    ]);
    out.extend_from_slice(&total_len.to_le_bytes()); // wTotalLength
    out.extend_from_slice(&[
        0x01, // bNumInterfaces
        0x01, // bConfigurationValue
        0x00, // iConfiguration
        0xa0, // bmAttributes (bus powered + remote wake)
        50,   // bMaxPower (100mA)
        // Interface descriptor
        0x09, // bLength
        DESC_INTERFACE,
        0x00, // bInterfaceNumber
        0x00, // bAlternateSetting
        num_endpoints, // bNumEndpoints
        0x03,          // bInterfaceClass (HID)
        interface_subclass,
        interface_protocol,
        0x00, // iInterface
    ]);
    out.extend_from_slice(hid_descriptor);
    out.extend_from_slice(&[
        0x07, // bLength
        DESC_ENDPOINT,
        INTERRUPT_IN_EP_ADDR, // bEndpointAddress
        0x03,                // bmAttributes (Interrupt)
    ]);
    out.extend_from_slice(&max_packet_size.to_le_bytes()); // wMaxPacketSize
    out.push(0x0a); // bInterval (10ms)

    if has_interrupt_out {
        out.extend_from_slice(&[
            0x07, // bLength
            DESC_ENDPOINT,
            INTERRUPT_OUT_EP_ADDR, // bEndpointAddress
            0x03,                 // bmAttributes (Interrupt)
        ]);
        out.extend_from_slice(&max_packet_size.to_le_bytes()); // wMaxPacketSize
        out.push(0x0a); // bInterval (10ms)
    }

    debug_assert_eq!(out.len(), total_len as usize);
    out
}

fn report_descriptor_report_lengths(
    report_descriptor_bytes: &[u8],
) -> (bool, BTreeMap<u8, usize>, BTreeMap<u8, usize>, BTreeMap<u8, usize>) {
    let Ok(collections) = report_descriptor::parse_report_descriptor(report_descriptor_bytes) else {
        return (
            report_descriptor_uses_report_ids(report_descriptor_bytes),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
    };

    let mut report_ids_in_use = false;
    let mut input_bits: BTreeMap<u8, u64> = BTreeMap::new();
    let mut output_bits: BTreeMap<u8, u64> = BTreeMap::new();
    let mut feature_bits: BTreeMap<u8, u64> = BTreeMap::new();

    for collection in &collections {
        accumulate_report_bits(
            collection,
            &mut report_ids_in_use,
            &mut input_bits,
            &mut output_bits,
            &mut feature_bits,
        );
    }

    (
        report_ids_in_use,
        bits_to_report_lengths(&input_bits),
        bits_to_report_lengths(&output_bits),
        bits_to_report_lengths(&feature_bits),
    )
}

fn bits_to_report_lengths(bits: &BTreeMap<u8, u64>) -> BTreeMap<u8, usize> {
    let mut out = BTreeMap::new();
    for (&report_id, &total_bits) in bits {
        let mut bytes = ((total_bits + 7) / 8) as usize;
        if report_id != 0 {
            bytes = bytes.saturating_add(1);
        }
        out.insert(report_id, bytes);
    }
    out
}

fn accumulate_report_bits(
    collection: &report_descriptor::HidCollectionInfo,
    report_ids_in_use: &mut bool,
    input_bits: &mut BTreeMap<u8, u64>,
    output_bits: &mut BTreeMap<u8, u64>,
    feature_bits: &mut BTreeMap<u8, u64>,
) {
    for report in &collection.input_reports {
        let Ok(report_id) = u8::try_from(report.report_id) else {
            *report_ids_in_use = true;
            continue;
        };
        if report_id != 0 {
            *report_ids_in_use = true;
        }
        add_bits(input_bits, report_id, report_bits(report));
    }
    for report in &collection.output_reports {
        let Ok(report_id) = u8::try_from(report.report_id) else {
            *report_ids_in_use = true;
            continue;
        };
        if report_id != 0 {
            *report_ids_in_use = true;
        }
        add_bits(output_bits, report_id, report_bits(report));
    }
    for report in &collection.feature_reports {
        let Ok(report_id) = u8::try_from(report.report_id) else {
            *report_ids_in_use = true;
            continue;
        };
        if report_id != 0 {
            *report_ids_in_use = true;
        }
        add_bits(feature_bits, report_id, report_bits(report));
    }

    for child in &collection.children {
        accumulate_report_bits(
            child,
            report_ids_in_use,
            input_bits,
            output_bits,
            feature_bits,
        );
    }
}

fn add_bits(map: &mut BTreeMap<u8, u64>, report_id: u8, bits: u64) {
    let entry = map.entry(report_id).or_insert(0);
    *entry = entry.saturating_add(bits);
}

fn report_bits(report: &report_descriptor::HidReportInfo) -> u64 {
    report
        .items
        .iter()
        .map(|item| u64::from(item.bit_len()))
        .fold(0u64, |acc, v| acc.saturating_add(v))
}

fn report_descriptor_uses_report_ids(report_descriptor: &[u8]) -> bool {
    let mut i = 0usize;
    while i < report_descriptor.len() {
        let b = report_descriptor[i];
        i += 1;
        if b == 0xFE {
            // Long item: bSize, bTag, data...
            if i + 2 > report_descriptor.len() {
                break;
            }
            let size = report_descriptor[i] as usize;
            i += 2;
            i = i.saturating_add(size);
            continue;
        }

        let size = match b & 0x03 {
            0 => 0usize,
            1 => 1usize,
            2 => 2usize,
            3 => 4usize,
            _ => 0usize,
        };

        // Global item, tag 8 = Report ID.
        if b & 0xFC == 0x84 {
            if size == 0 {
                return true;
            }
            if i + size > report_descriptor.len() {
                break;
            }
            let mut value: u32 = 0;
            for (shift, byte) in report_descriptor[i..i + size].iter().enumerate() {
                value |= (*byte as u32) << (shift * 8);
            }
            if value != 0 {
                return true;
            }
        }

        i = i.saturating_add(size);
    }
    false
}
