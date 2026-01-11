use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use crate::io::usb::core::UsbOutResult;
use crate::io::usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

use super::{
    build_string_descriptor_utf16le, clamp_response, HidProtocol, HID_REQUEST_GET_IDLE,
    HID_REQUEST_GET_PROTOCOL, HID_REQUEST_GET_REPORT, HID_REQUEST_SET_IDLE,
    HID_REQUEST_SET_PROTOCOL, HID_REQUEST_SET_REPORT, USB_DESCRIPTOR_TYPE_CONFIGURATION,
    USB_DESCRIPTOR_TYPE_DEVICE, USB_DESCRIPTOR_TYPE_HID, USB_DESCRIPTOR_TYPE_HID_REPORT,
    USB_DESCRIPTOR_TYPE_STRING, USB_FEATURE_DEVICE_REMOTE_WAKEUP, USB_FEATURE_ENDPOINT_HALT,
    USB_REQUEST_CLEAR_FEATURE, USB_REQUEST_GET_CONFIGURATION, USB_REQUEST_GET_DESCRIPTOR,
    USB_REQUEST_GET_INTERFACE, USB_REQUEST_GET_STATUS, USB_REQUEST_SET_ADDRESS,
    USB_REQUEST_SET_CONFIGURATION, USB_REQUEST_SET_FEATURE, USB_REQUEST_SET_INTERFACE,
};

const INTERRUPT_IN_EP: u8 = 0x81;
const INTERRUPT_OUT_EP: u8 = 0x01;

const DEFAULT_MAX_PACKET_SIZE: u16 = 64;
const DEFAULT_MAX_PENDING_INPUT_REPORTS: usize = 256;
const DEFAULT_MAX_PENDING_OUTPUT_REPORTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbHidPassthroughOutputReport {
    /// HID report type as used by GET_REPORT/SET_REPORT:
    /// 2 = Output, 3 = Feature.
    pub report_type: u8,
    pub report_id: u8,
    /// Report payload (without the report ID prefix).
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct UsbHidPassthrough {
    address: u8,
    configuration: u8,
    remote_wakeup_enabled: bool,
    interrupt_in_halted: bool,
    interrupt_out_halted: bool,
    idle_rate: u8,
    protocol: HidProtocol,

    device_descriptor: Rc<[u8]>,
    config_descriptor: Rc<[u8]>,
    hid_descriptor: Rc<[u8]>,
    hid_report_descriptor: Rc<[u8]>,
    manufacturer_string_descriptor: Rc<[u8]>,
    product_string_descriptor: Rc<[u8]>,
    serial_string_descriptor: Option<Rc<[u8]>>,

    has_interrupt_out: bool,
    report_ids_in_use: bool,
    max_pending_input_reports: usize,
    max_pending_output_reports: usize,

    pending_input_reports: VecDeque<Vec<u8>>,
    last_input_reports: HashMap<u8, Vec<u8>>,
    pending_output_reports: VecDeque<UsbHidPassthroughOutputReport>,
}

/// Shareable handle for a USB HID passthrough device model.
#[derive(Clone, Debug)]
pub struct UsbHidPassthroughHandle {
    inner: Rc<RefCell<UsbHidPassthrough>>,
    device_descriptor: Rc<[u8]>,
    config_descriptor: Rc<[u8]>,
    hid_report_descriptor: Rc<[u8]>,
}

impl UsbHidPassthroughHandle {
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
        let model = UsbHidPassthrough::new(
            vendor_id,
            product_id,
            manufacturer,
            product,
            serial,
            hid_report_descriptor,
            has_interrupt_out,
            max_packet_size.unwrap_or(DEFAULT_MAX_PACKET_SIZE),
            interface_subclass.unwrap_or(0),
            interface_protocol.unwrap_or(0),
        );

        let device_descriptor = model.device_descriptor.clone();
        let config_descriptor = model.config_descriptor.clone();
        let hid_report_descriptor = model.hid_report_descriptor.clone();

        Self {
            inner: Rc::new(RefCell::new(model)),
            device_descriptor,
            config_descriptor,
            hid_report_descriptor,
        }
    }

    pub fn configured(&self) -> bool {
        self.inner.borrow().configuration != 0
    }

    pub fn push_input_report(&self, report_id: u8, data: &[u8]) {
        self.inner.borrow_mut().push_input_report(report_id, data);
    }

    pub fn pop_output_report(&self) -> Option<UsbHidPassthroughOutputReport> {
        self.inner.borrow_mut().pending_output_reports.pop_front()
    }
}

impl UsbDeviceModel for UsbHidPassthroughHandle {
    fn get_device_descriptor(&self) -> &[u8] {
        self.device_descriptor.as_ref()
    }

    fn get_config_descriptor(&self) -> &[u8] {
        self.config_descriptor.as_ref()
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        self.hid_report_descriptor.as_ref()
    }

    fn reset(&mut self) {
        self.inner.borrow_mut().reset();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        self.inner
            .borrow_mut()
            .handle_control_request(setup, data_stage)
    }

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        self.inner.borrow_mut().poll_interrupt_in(ep)
    }

    fn handle_interrupt_out(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        self.inner.borrow_mut().handle_interrupt_out(ep, data)
    }
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
        max_packet_size: u16,
        interface_subclass: u8,
        interface_protocol: u8,
    ) -> Self {
        let max_packet_size = sanitize_max_packet_size(max_packet_size);

        let manufacturer_string_descriptor: Rc<[u8]> =
            Rc::from(build_string_descriptor_utf16le(&manufacturer).into_boxed_slice());
        let product_string_descriptor: Rc<[u8]> =
            Rc::from(build_string_descriptor_utf16le(&product).into_boxed_slice());
        let serial_string_descriptor = serial
            .as_deref()
            .map(build_string_descriptor_utf16le)
            .map(|v| Rc::<[u8]>::from(v.into_boxed_slice()));

        let hid_report_descriptor: Rc<[u8]> =
            Rc::from(hid_report_descriptor.into_boxed_slice());

        let i_serial = if serial_string_descriptor.is_some() { 3 } else { 0 };

        let device_descriptor: Rc<[u8]> = Rc::from(
            build_device_descriptor(
                vendor_id,
                product_id,
                max_packet_size as u8,
                1,
                2,
                i_serial,
            )
            .into_boxed_slice(),
        );

        let hid_descriptor: Rc<[u8]> = Rc::from(
            build_hid_descriptor(hid_report_descriptor.as_ref()).into_boxed_slice(),
        );
        let config_descriptor: Rc<[u8]> = Rc::from(
            build_config_descriptor(
                hid_descriptor.as_ref(),
                has_interrupt_out,
                max_packet_size,
                interface_subclass,
                interface_protocol,
            )
            .into_boxed_slice(),
        );

        let report_ids_in_use = report_descriptor_uses_report_ids(hid_report_descriptor.as_ref());

        Self {
            address: 0,
            configuration: 0,
            remote_wakeup_enabled: false,
            interrupt_in_halted: false,
            interrupt_out_halted: false,
            idle_rate: 0,
            protocol: HidProtocol::Report,
            device_descriptor,
            config_descriptor,
            hid_descriptor,
            hid_report_descriptor,
            manufacturer_string_descriptor,
            product_string_descriptor,
            serial_string_descriptor,
            has_interrupt_out,
            report_ids_in_use,
            max_pending_input_reports: DEFAULT_MAX_PENDING_INPUT_REPORTS,
            max_pending_output_reports: DEFAULT_MAX_PENDING_OUTPUT_REPORTS,
            pending_input_reports: VecDeque::new(),
            last_input_reports: HashMap::new(),
            pending_output_reports: VecDeque::new(),
        }
    }

    pub fn push_input_report(&mut self, report_id: u8, data: &[u8]) {
        let mut out = Vec::with_capacity(data.len() + if report_id != 0 { 1 } else { 0 });
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

    fn push_output_report(&mut self, report: UsbHidPassthroughOutputReport) {
        if self.pending_output_reports.len() >= self.max_pending_output_reports {
            self.pending_output_reports.pop_front();
        }
        self.pending_output_reports.push_back(report);
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(self.manufacturer_string_descriptor.as_ref().to_vec()),
            2 => Some(self.product_string_descriptor.as_ref().to_vec()),
            3 => self
                .serial_string_descriptor
                .as_ref()
                .map(|d| d.as_ref().to_vec()),
            _ => None,
        }
    }
}

impl UsbDeviceModel for UsbHidPassthrough {
    fn get_device_descriptor(&self) -> &[u8] {
        self.device_descriptor.as_ref()
    }

    fn get_config_descriptor(&self) -> &[u8] {
        self.config_descriptor.as_ref()
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        self.hid_report_descriptor.as_ref()
    }

    fn reset(&mut self) {
        self.address = 0;
        self.configuration = 0;
        self.remote_wakeup_enabled = false;
        self.interrupt_in_halted = false;
        self.interrupt_out_halted = false;
        self.idle_rate = 0;
        self.protocol = HidProtocol::Report;
        self.pending_input_reports.clear();
        self.pending_output_reports.clear();
        self.last_input_reports.clear();
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
                            || setup.w_length != 0
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
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let config = (setup.w_value & 0x00ff) as u8;
                    if config > 1 {
                        return ControlResponse::Stall;
                    }
                    self.configuration = config;
                    if self.configuration == 0 {
                        self.pending_input_reports.clear();
                        self.pending_output_reports.clear();
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
            (RequestType::Standard, RequestRecipient::Interface) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
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
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let desc_type = setup.descriptor_type();
                    let data = match desc_type {
                        USB_DESCRIPTOR_TYPE_HID_REPORT => {
                            Some(self.get_hid_report_descriptor().to_vec())
                        }
                        USB_DESCRIPTOR_TYPE_HID => Some(self.hid_descriptor.as_ref().to_vec()),
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
                    let halted = match setup.w_index as u8 {
                        INTERRUPT_IN_EP => self.interrupt_in_halted,
                        INTERRUPT_OUT_EP if self.has_interrupt_out => self.interrupt_out_halted,
                        _ => return ControlResponse::Stall,
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
                    if setup.w_value != USB_FEATURE_ENDPOINT_HALT {
                        return ControlResponse::Stall;
                    }
                    match setup.w_index as u8 {
                        INTERRUPT_IN_EP => {
                            self.interrupt_in_halted = false;
                            ControlResponse::Ack
                        }
                        INTERRUPT_OUT_EP if self.has_interrupt_out => {
                            self.interrupt_out_halted = false;
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
                    }
                }
                USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value != USB_FEATURE_ENDPOINT_HALT {
                        return ControlResponse::Stall;
                    }
                    match setup.w_index as u8 {
                        INTERRUPT_IN_EP => {
                            self.interrupt_in_halted = true;
                            ControlResponse::Ack
                        }
                        INTERRUPT_OUT_EP if self.has_interrupt_out => {
                            self.interrupt_out_halted = true;
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
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
                    let report_id = (setup.w_value & 0x00ff) as u8;
                    let data = match report_type {
                        1 => self
                            .last_input_reports
                            .get(&report_id)
                            .cloned()
                            .unwrap_or_else(|| default_input_report(report_id, setup.w_length)),
                        // Minimal behavior for output/feature: return zeros.
                        2 | 3 => vec![0; setup.w_length.min(64) as usize],
                        _ => return ControlResponse::Stall,
                    };
                    ControlResponse::Data(clamp_response(data, setup.w_length))
                }
                HID_REQUEST_SET_REPORT => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let report_type = (setup.w_value >> 8) as u8;
                    let report_id = (setup.w_value & 0x00ff) as u8;
                    match (report_type, data_stage) {
                        (2 | 3, Some(data)) => {
                            self.push_output_report(UsbHidPassthroughOutputReport {
                                report_type,
                                report_id,
                                data: data.to_vec(),
                            });
                            ControlResponse::Ack
                        }
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

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        if ep != INTERRUPT_IN_EP {
            return None;
        }
        if self.configuration == 0 || self.interrupt_in_halted {
            return None;
        }
        self.pending_input_reports.pop_front()
    }

    fn handle_interrupt_out(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        if ep != INTERRUPT_OUT_EP || !self.has_interrupt_out {
            return UsbOutResult::Stall;
        }
        if self.configuration == 0 || self.interrupt_out_halted {
            return UsbOutResult::Stall;
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
        UsbOutResult::Ack
    }
}

fn default_input_report(report_id: u8, w_length: u16) -> Vec<u8> {
    let len = w_length.min(4096) as usize;
    if len == 0 {
        return Vec::new();
    }
    let mut data = vec![0u8; len];
    if report_id != 0 {
        data[0] = report_id;
    }
    data
}

fn sanitize_max_packet_size(max_packet_size: u16) -> u16 {
    match max_packet_size {
        8 | 16 | 32 | 64 => max_packet_size,
        _ => DEFAULT_MAX_PACKET_SIZE,
    }
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
        USB_DESCRIPTOR_TYPE_DEVICE,
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
        0x09,                    // bLength
        USB_DESCRIPTOR_TYPE_HID, // bDescriptorType
        0x11,
        0x01,                           // bcdHID (1.11)
        0x00,                           // bCountryCode
        0x01,                           // bNumDescriptors
        USB_DESCRIPTOR_TYPE_HID_REPORT, // bDescriptorType (Report)
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
    let total_len = 9u16 + 9u16 + hid_descriptor.len() as u16 + 7u16 + if has_interrupt_out { 7 } else { 0 };
    let num_endpoints = if has_interrupt_out { 2 } else { 1 };

    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&[
        0x09, // bLength
        USB_DESCRIPTOR_TYPE_CONFIGURATION,
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
        super::USB_DESCRIPTOR_TYPE_INTERFACE,
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
        super::USB_DESCRIPTOR_TYPE_ENDPOINT,
        INTERRUPT_IN_EP, // bEndpointAddress
        0x03,            // bmAttributes (Interrupt)
    ]);
    out.extend_from_slice(&max_packet_size.to_le_bytes()); // wMaxPacketSize
    out.push(0x0a); // bInterval (10ms)

    if has_interrupt_out {
        out.extend_from_slice(&[
            0x07, // bLength
            super::USB_DESCRIPTOR_TYPE_ENDPOINT,
            INTERRUPT_OUT_EP, // bEndpointAddress
            0x03,             // bmAttributes (Interrupt)
        ]);
        out.extend_from_slice(&max_packet_size.to_le_bytes()); // wMaxPacketSize
        out.push(0x0a); // bInterval (10ms)
    }

    debug_assert_eq!(out.len(), total_len as usize);
    out
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
            return true;
        }

        i = i.saturating_add(size);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn configure_device(dev: &mut UsbHidPassthroughHandle) {
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

    fn sample_report_descriptor_with_ids() -> Vec<u8> {
        vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x00, // Usage (Undefined)
            0xa1, 0x01, // Collection (Application)
            0x85, 0x01, // Report ID (1)
            0x09, 0x00, // Usage (Undefined)
            0x15, 0x00, // Logical Minimum (0)
            0x26, 0xff, 0x00, // Logical Maximum (255)
            0x75, 0x08, // Report Size (8)
            0x95, 0x04, // Report Count (4)
            0x81, 0x02, // Input (Data,Var,Abs)
            0xc0, // End Collection
        ]
    }

    #[test]
    fn descriptors_are_well_formed() {
        let report = sample_report_descriptor_with_ids();
        let dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".to_string(),
            "Product".to_string(),
            Some("Serial".to_string()),
            report.clone(),
            true,
            None,
            Some(1),
            Some(1),
        );

        let device_desc = dev.get_device_descriptor();
        assert_eq!(device_desc.len(), 18);
        assert_eq!(device_desc[0] as usize, device_desc.len());
        assert_eq!(device_desc[1], USB_DESCRIPTOR_TYPE_DEVICE);

        let cfg = dev.get_config_descriptor();
        assert_eq!(cfg[0], 0x09);
        assert_eq!(cfg[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(w_le(&cfg, 2) as usize, cfg.len());

        // HID descriptor starts at offset 18 (9 config + 9 interface).
        let hid = &cfg[18..27];
        assert_eq!(hid[0], 0x09);
        assert_eq!(hid[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(hid[6], USB_DESCRIPTOR_TYPE_HID_REPORT);
        assert_eq!(w_le(hid, 7) as usize, report.len());

        // Endpoint IN is always present; OUT is present when requested.
        let ep_in = &cfg[27..34];
        assert_eq!(ep_in[1], super::super::USB_DESCRIPTOR_TYPE_ENDPOINT);
        assert_eq!(ep_in[2], INTERRUPT_IN_EP);

        let ep_out = &cfg[34..41];
        assert_eq!(ep_out[2], INTERRUPT_OUT_EP);
    }

    #[test]
    fn push_input_report_and_poll_interrupt_in_prefixes_report_id() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".to_string(),
            "Product".to_string(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        dev.push_input_report(1, &[0xaa, 0xbb, 0xcc]);
        assert_eq!(dev.poll_interrupt_in(INTERRUPT_IN_EP), None);

        configure_device(&mut dev);
        assert_eq!(
            dev.poll_interrupt_in(INTERRUPT_IN_EP).unwrap(),
            vec![1, 0xaa, 0xbb, 0xcc]
        );

        dev.push_input_report(0, &[0x11, 0x22]);
        assert_eq!(
            dev.poll_interrupt_in(INTERRUPT_IN_EP).unwrap(),
            vec![0x11, 0x22]
        );
    }

    #[test]
    fn set_report_and_interrupt_out_are_queued() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".to_string(),
            "Product".to_string(),
            None,
            report,
            true,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // SET_REPORT (Feature)
        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_REPORT,
                    w_value: (3u16 << 8) | 7u16, // Feature, report ID 7
                    w_index: 0,
                    w_length: 3,
                },
                Some(&[0xde, 0xad, 0xbe]),
            ),
            ControlResponse::Ack
        );

        // Interrupt OUT report: report ID prefix should be parsed when report IDs are in use.
        assert_eq!(
            dev.handle_interrupt_out(0x01, &[9, 0x01, 0x02]),
            UsbOutResult::Ack
        );

        let r1 = dev.pop_output_report().unwrap();
        assert_eq!(
            r1,
            UsbHidPassthroughOutputReport {
                report_type: 3,
                report_id: 7,
                data: vec![0xde, 0xad, 0xbe]
            }
        );

        let r2 = dev.pop_output_report().unwrap();
        assert_eq!(
            r2,
            UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 9,
                data: vec![0x01, 0x02]
            }
        );
    }

    #[test]
    fn input_and_output_queues_are_bounded() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".to_string(),
            "Product".to_string(),
            None,
            report,
            true,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // Overflow input queue.
        for i in 0..(DEFAULT_MAX_PENDING_INPUT_REPORTS + 50) {
            dev.push_input_report(1, &[i as u8]);
        }
        assert!(
            dev.inner.borrow().pending_input_reports.len() <= DEFAULT_MAX_PENDING_INPUT_REPORTS
        );

        // Drain and ensure the oldest entries were dropped.
        let mut last = None;
        while let Some(r) = dev.poll_interrupt_in(INTERRUPT_IN_EP) {
            last = Some(r);
        }
        assert_eq!(last.unwrap(), vec![1, (DEFAULT_MAX_PENDING_INPUT_REPORTS + 49) as u8]);

        // Overflow output queue.
        for i in 0..(DEFAULT_MAX_PENDING_OUTPUT_REPORTS + 17) {
            assert_eq!(
                dev.handle_interrupt_out(0x01, &[1, i as u8]),
                UsbOutResult::Ack
            );
        }
        assert!(
            dev.inner.borrow().pending_output_reports.len() <= DEFAULT_MAX_PENDING_OUTPUT_REPORTS
        );

        let mut last_out = None;
        while let Some(r) = dev.pop_output_report() {
            last_out = Some(r);
        }
        assert_eq!(
            last_out.unwrap(),
            UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 1,
                data: vec![(DEFAULT_MAX_PENDING_OUTPUT_REPORTS + 16) as u8]
            }
        );
    }
}
