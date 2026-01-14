use core::cell::RefCell;

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::device::{UsbInResult, UsbOutResult};
use crate::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

use super::report_descriptor;
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

/// Upper bound for host-provided input reports when the report ID is not present in the parsed
/// descriptor.
///
/// Input reports arrive from external injection paths (e.g. WebHID). When the report ID cannot be
/// matched to a descriptor-defined report size, cap the stored report bytes to avoid unbounded
/// allocations.
const MAX_UNKNOWN_INPUT_REPORT_BYTES: usize = DEFAULT_MAX_PACKET_SIZE as usize;

/// Upper bound for guest-provided `SET_REPORT` / interrupt OUT payloads when the report ID is not
/// present in the parsed descriptor.
///
/// Without this, an untrusted guest can send `SET_REPORT` with a very large `wLength` (up to
/// 65535), forcing the device model to allocate and queue arbitrarily large `Vec<u8>` instances.
const MAX_HID_SET_REPORT_BYTES: usize = 4 * 1024;

// Snapshot metadata used to reconstruct passthrough devices without requiring the host to
// pre-attach them before restoring a UHCI controller snapshot. These fields encode the static
// descriptor/report configuration and therefore allow recreating the model instance.
const HIDP_SNAP_TAG_VENDOR_ID: u16 = 15;
const HIDP_SNAP_TAG_PRODUCT_ID: u16 = 16;
const HIDP_SNAP_TAG_MANUFACTURER: u16 = 17;
const HIDP_SNAP_TAG_PRODUCT: u16 = 18;
const HIDP_SNAP_TAG_SERIAL: u16 = 19;
const HIDP_SNAP_TAG_HID_REPORT_DESCRIPTOR: u16 = 20;
const HIDP_SNAP_TAG_HAS_INTERRUPT_OUT: u16 = 21;
const HIDP_SNAP_TAG_MAX_PACKET_SIZE: u16 = 22;
const HIDP_SNAP_TAG_INTERFACE_SUBCLASS: u16 = 23;
const HIDP_SNAP_TAG_INTERFACE_PROTOCOL: u16 = 24;

fn decode_string_descriptor_utf16le(desc: &[u8]) -> Option<String> {
    if desc.len() < 2 {
        return None;
    }
    if desc[1] != USB_DESCRIPTOR_TYPE_STRING {
        return None;
    }
    let len = desc[0] as usize;
    if len < 2 || len > desc.len() {
        return None;
    }
    let payload = &desc[2..len];
    if !payload.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

fn parse_device_descriptor_fields(bytes: &[u8]) -> Option<(u16, u16, u16)> {
    if bytes.len() < 12 {
        return None;
    }
    let max_packet_size = bytes[7] as u16;
    let vendor_id = u16::from_le_bytes([bytes[8], bytes[9]]);
    let product_id = u16::from_le_bytes([bytes[10], bytes[11]]);
    Some((vendor_id, product_id, max_packet_size))
}

fn parse_interface_descriptor_fields(bytes: &[u8]) -> Option<(u8, u8)> {
    const INTERFACE_DESC_OFFSET: usize = 9;
    if bytes.len() < INTERFACE_DESC_OFFSET + 9 {
        return None;
    }
    // Config descriptor is always followed immediately by a single interface descriptor.
    if bytes[INTERFACE_DESC_OFFSET] != 0x09
        || bytes[INTERFACE_DESC_OFFSET + 1] != super::USB_DESCRIPTOR_TYPE_INTERFACE
    {
        return None;
    }
    let subclass = bytes[INTERFACE_DESC_OFFSET + 6];
    let protocol = bytes[INTERFACE_DESC_OFFSET + 7];
    Some((subclass, protocol))
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

/// Host-side request emitted when the guest issues a `GET_REPORT (Feature)` class request.
///
/// The host runtime (e.g. WebHID) should service this by calling
/// `UsbHidPassthroughHandle::complete_feature_report_request` or
/// `UsbHidPassthroughHandle::fail_feature_report_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbHidPassthroughFeatureReportRequest {
    pub request_id: u32,
    pub report_id: u8,
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
    input_report_lengths: BTreeMap<u8, usize>,
    output_report_lengths: BTreeMap<u8, usize>,
    feature_report_lengths: BTreeMap<u8, usize>,
    max_pending_input_reports: usize,
    max_pending_output_reports: usize,

    pending_input_reports: VecDeque<Vec<u8>>,
    last_input_reports: BTreeMap<u8, Vec<u8>>,
    last_output_reports: BTreeMap<u8, Vec<u8>>,
    last_feature_reports: BTreeMap<u8, Vec<u8>>,
    cached_feature_reports: BTreeMap<u8, Vec<u8>>,
    pending_output_reports: VecDeque<UsbHidPassthroughOutputReport>,

    next_feature_report_request_id: u32,
    feature_report_request_queue: VecDeque<UsbHidPassthroughFeatureReportRequest>,
    feature_report_requests_pending: BTreeMap<u8, u32>,
    feature_report_requests_failed: BTreeMap<u8, u32>,
}

/// Shareable handle for a USB HID passthrough device model.
#[derive(Clone, Debug)]
pub struct UsbHidPassthroughHandle {
    inner: Rc<RefCell<UsbHidPassthrough>>,
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

        Self {
            inner: Rc::new(RefCell::new(model)),
        }
    }

    pub(crate) fn try_new_from_snapshot(bytes: &[u8]) -> SnapshotResult<Option<Self>> {
        const MAX_STRING_BYTES: usize = 16 * 1024;
        const MAX_REPORT_DESCRIPTOR_BYTES: usize = 1024 * 1024;

        let r = SnapshotReader::parse(bytes, UsbHidPassthrough::DEVICE_ID)?;
        r.ensure_device_major(UsbHidPassthrough::DEVICE_VERSION.major)?;

        let Some(vendor_id) = r.u16(HIDP_SNAP_TAG_VENDOR_ID)? else {
            return Ok(None);
        };
        let Some(product_id) = r.u16(HIDP_SNAP_TAG_PRODUCT_ID)? else {
            return Ok(None);
        };
        let Some(has_interrupt_out) = r.bool(HIDP_SNAP_TAG_HAS_INTERRUPT_OUT)? else {
            return Ok(None);
        };
        let Some(report_desc) = r.bytes(HIDP_SNAP_TAG_HID_REPORT_DESCRIPTOR) else {
            return Ok(None);
        };
        if report_desc.len() > MAX_REPORT_DESCRIPTOR_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding(
                "hid report descriptor too large",
            ));
        }

        let Some(manufacturer) = r.bytes(HIDP_SNAP_TAG_MANUFACTURER) else {
            return Ok(None);
        };
        if manufacturer.len() > MAX_STRING_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding(
                "manufacturer too large",
            ));
        }
        let manufacturer = String::from_utf8(manufacturer.to_vec())
            .map_err(|_| SnapshotError::InvalidFieldEncoding("manufacturer"))?;

        let Some(product) = r.bytes(HIDP_SNAP_TAG_PRODUCT) else {
            return Ok(None);
        };
        if product.len() > MAX_STRING_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding("product too large"));
        }
        let product = String::from_utf8(product.to_vec())
            .map_err(|_| SnapshotError::InvalidFieldEncoding("product"))?;

        let serial = match r.bytes(HIDP_SNAP_TAG_SERIAL) {
            Some(bytes) => {
                if bytes.len() > MAX_STRING_BYTES {
                    return Err(SnapshotError::InvalidFieldEncoding("serial too large"));
                }
                Some(
                    String::from_utf8(bytes.to_vec())
                        .map_err(|_| SnapshotError::InvalidFieldEncoding("serial"))?,
                )
            }
            None => None,
        };

        let max_packet_size = r.u16(HIDP_SNAP_TAG_MAX_PACKET_SIZE)?;
        let interface_subclass = r.u8(HIDP_SNAP_TAG_INTERFACE_SUBCLASS)?;
        let interface_protocol = r.u8(HIDP_SNAP_TAG_INTERFACE_PROTOCOL)?;

        Ok(Some(Self::new(
            vendor_id,
            product_id,
            manufacturer,
            product,
            serial,
            report_desc.to_vec(),
            has_interrupt_out,
            max_packet_size,
            interface_subclass,
            interface_protocol,
        )))
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

    /// Drain the next pending host-side `GET_REPORT (Feature)` request issued by the guest.
    pub fn pop_feature_report_request(&self) -> Option<UsbHidPassthroughFeatureReportRequest> {
        self.inner
            .borrow_mut()
            .feature_report_request_queue
            .pop_front()
    }

    /// Complete a pending feature report request with the report payload bytes.
    ///
    /// `data` should NOT include the report ID prefix; the device model will add it when
    /// `report_id != 0` to match USB HID `GET_REPORT` semantics expected by Windows.
    pub fn complete_feature_report_request(
        &self,
        request_id: u32,
        report_id: u8,
        data: &[u8],
    ) -> bool {
        self.inner
            .borrow_mut()
            .complete_feature_report_request(request_id, report_id, data)
    }

    /// Fail a pending feature report request.
    ///
    /// The guest control transfer will complete with a timeout-style error on the next poll.
    pub fn fail_feature_report_request(&self, request_id: u32, report_id: u8) -> bool {
        self.inner
            .borrow_mut()
            .fail_feature_report_request(request_id, report_id)
    }

    pub fn set_max_pending_input_reports(&self, max: usize) {
        self.inner.borrow_mut().set_max_pending_input_reports(max);
    }

    pub fn set_max_pending_output_reports(&self, max: usize) {
        self.inner.borrow_mut().set_max_pending_output_reports(max);
    }
}

impl UsbDeviceModel for UsbHidPassthroughHandle {
    fn reset(&mut self) {
        self.inner.borrow_mut().reset();
    }

    fn cancel_control_transfer(&mut self) {
        self.inner.borrow_mut().cancel_control_transfer();
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

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        self.inner.borrow_mut().handle_interrupt_in(ep_addr)
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
        mut hid_report_descriptor: Vec<u8>,
        has_interrupt_out: bool,
        max_packet_size: u16,
        interface_subclass: u8,
        interface_protocol: u8,
    ) -> Self {
        let max_packet_size = sanitize_max_packet_size(max_packet_size);

        // USB HID encodes the report descriptor length as a u16 (wDescriptorLength). Truncate
        // oversized descriptors so the device's own descriptors remain self-consistent.
        if hid_report_descriptor.len() > u16::MAX as usize {
            hid_report_descriptor.truncate(u16::MAX as usize);
            hid_report_descriptor.shrink_to_fit();
        }

        let manufacturer_string_descriptor: Rc<[u8]> =
            Rc::from(build_string_descriptor_utf16le(&manufacturer).into_boxed_slice());
        let product_string_descriptor: Rc<[u8]> =
            Rc::from(build_string_descriptor_utf16le(&product).into_boxed_slice());
        let serial_string_descriptor = serial
            .as_deref()
            .map(build_string_descriptor_utf16le)
            .map(|v| Rc::<[u8]>::from(v.into_boxed_slice()));

        let hid_report_descriptor: Rc<[u8]> = Rc::from(hid_report_descriptor.into_boxed_slice());

        let i_serial = if serial_string_descriptor.is_some() {
            3
        } else {
            0
        };

        let device_descriptor: Rc<[u8]> = Rc::from(
            build_device_descriptor(vendor_id, product_id, max_packet_size as u8, 1, 2, i_serial)
                .into_boxed_slice(),
        );

        let hid_descriptor: Rc<[u8]> =
            Rc::from(build_hid_descriptor(hid_report_descriptor.as_ref()).into_boxed_slice());
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

        let (
            report_ids_in_use,
            input_report_lengths,
            output_report_lengths,
            feature_report_lengths,
        ) = report_descriptor_report_lengths(hid_report_descriptor.as_ref());

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
            input_report_lengths,
            output_report_lengths,
            feature_report_lengths,
            max_pending_input_reports: DEFAULT_MAX_PENDING_INPUT_REPORTS,
            max_pending_output_reports: DEFAULT_MAX_PENDING_OUTPUT_REPORTS,
            pending_input_reports: VecDeque::new(),
            last_input_reports: BTreeMap::new(),
            last_output_reports: BTreeMap::new(),
            last_feature_reports: BTreeMap::new(),
            cached_feature_reports: BTreeMap::new(),
            pending_output_reports: VecDeque::new(),

            next_feature_report_request_id: 1,
            feature_report_request_queue: VecDeque::new(),
            feature_report_requests_pending: BTreeMap::new(),
            feature_report_requests_failed: BTreeMap::new(),
        }
    }

    pub fn push_input_report(&mut self, report_id: u8, data: &[u8]) {
        let out = match self.input_report_lengths.get(&report_id).copied() {
            Some(expected_len) => {
                if expected_len == 0 {
                    Vec::new()
                } else if report_id == 0 {
                    let mut out = vec![0u8; expected_len];
                    let copy_len = data.len().min(expected_len);
                    out[..copy_len].copy_from_slice(&data[..copy_len]);
                    out
                } else {
                    let mut out = vec![0u8; expected_len];
                    out[0] = report_id;

                    // WebHID is expected to provide the report ID separately (via `reportId`) and
                    // the report payload without the report ID prefix (via `data`). However, some
                    // host APIs include the report ID prefix in `data` anyway; in that case avoid
                    // double-prefixing and generating `[id, id, ...]`.
                    //
                    // Only strip the prefix when the provided bytes match the descriptor-derived
                    // total report length, mirroring the SET_REPORT double-prefix protection.
                    let payload =
                        if expected_len == data.len() && data.first() == Some(&report_id) {
                            &data[1..]
                        } else {
                            data
                        };

                    let payload_len = expected_len.saturating_sub(1);
                    let copy_len = payload.len().min(payload_len);
                    if copy_len != 0 {
                        out[1..1 + copy_len].copy_from_slice(&payload[..copy_len]);
                    }
                    out
                }
            }
            None => {
                // Unknown report ID: cap allocations.
                let mut out = Vec::with_capacity(
                    data.len()
                        .saturating_add(usize::from(report_id != 0))
                        .min(MAX_UNKNOWN_INPUT_REPORT_BYTES),
                );
                if report_id != 0 {
                    out.push(report_id);
                }
                let remaining = MAX_UNKNOWN_INPUT_REPORT_BYTES.saturating_sub(out.len());
                let copy_len = data.len().min(remaining);
                out.extend_from_slice(&data[..copy_len]);
                out
            }
        };

        self.last_input_reports.insert(report_id, out.clone());

        // USB interrupt endpoints are not active until the device has been configured. Input
        // reports that arrive before `SET_CONFIGURATION` completes should not be queued and later
        // replayed as stale events. Instead, keep only the last image per report ID and seed it
        // once the device is configured.
        if self.configuration == 0 {
            return;
        }

        if self.pending_input_reports.len() >= self.max_pending_input_reports {
            self.pending_input_reports.pop_front();
        }
        self.pending_input_reports.push_back(out);
    }

    fn seed_input_reports_on_configuration(&mut self) {
        let relative_ranges = scan_relative_input_bit_ranges(&self.hid_report_descriptor);

        for (&report_id, report) in &self.last_input_reports {
            let mut seeded = report.clone();

            let base = usize::from(report_id != 0) * 8;
            if let Some(ranges) = relative_ranges.get(&report_id) {
                for &(start, len) in ranges {
                    clear_bits(
                        &mut seeded,
                        base.saturating_add(start as usize),
                        len as usize,
                    );
                }
            }

            // Only enqueue reports that represent a non-default state (keys/buttons held, etc).
            let mut default = vec![0u8; seeded.len()];
            if report_id != 0 && !default.is_empty() {
                default[0] = report_id;
            }
            if seeded == default {
                continue;
            }

            if self.pending_input_reports.len() >= self.max_pending_input_reports {
                self.pending_input_reports.pop_front();
            }
            self.pending_input_reports.push_back(seeded);
        }
    }

    fn try_push_output_report(&mut self, report: UsbHidPassthroughOutputReport) -> bool {
        if self.pending_output_reports.len() >= self.max_pending_output_reports {
            return false;
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
                // Feature reports are used as configuration channels; once the guest mutates a
                // report via SET_REPORT we no longer know whether any previously cached host-provided
                // image is still valid.
                self.cached_feature_reports.remove(&report.report_id);
            }
            _ => {}
        }
        self.pending_output_reports.push_back(report);
        true
    }

    fn alloc_feature_report_request_id(&mut self) -> u32 {
        let id = self.next_feature_report_request_id;
        self.next_feature_report_request_id = self
            .next_feature_report_request_id
            .wrapping_add(1)
            .max(1);
        id
    }

    fn enqueue_feature_report_request(&mut self, report_id: u8) {
        if self.feature_report_requests_pending.contains_key(&report_id) {
            return;
        }
        let request_id = self.alloc_feature_report_request_id();
        self.feature_report_requests_pending.insert(report_id, request_id);
        self.feature_report_request_queue
            .push_back(UsbHidPassthroughFeatureReportRequest {
                request_id,
                report_id,
            });
    }

    fn complete_feature_report_request(
        &mut self,
        request_id: u32,
        report_id: u8,
        data: &[u8],
    ) -> bool {
        let Some(&pending_id) = self.feature_report_requests_pending.get(&report_id) else {
            return false;
        };
        if pending_id != request_id {
            return false;
        }

        self.feature_report_requests_pending.remove(&report_id);
        self.feature_report_requests_failed.remove(&report_id);
        self.feature_report_request_queue
            .retain(|req| req.request_id != request_id);

        let payload = self.normalize_report_payload_no_prefix(3, report_id, data);
        self.cached_feature_reports
            .insert(report_id, bytes_with_report_id(report_id, &payload));
        true
    }

    fn fail_feature_report_request(&mut self, request_id: u32, report_id: u8) -> bool {
        let Some(&pending_id) = self.feature_report_requests_pending.get(&report_id) else {
            return false;
        };
        if pending_id != request_id {
            return false;
        }
        self.feature_report_requests_failed.insert(report_id, request_id);
        self.feature_report_request_queue
            .retain(|req| req.request_id != request_id);
        true
    }

    fn cancel_control_transfer(&mut self) {
        // Feature report reads are serviced asynchronously by the host runtime (e.g. WebHID). When
        // the guest aborts a control transfer (by issuing a new SETUP) we should drop any queued or
        // in-flight requests so stale host completions cannot leak and unblock a future unrelated
        // transfer.
        self.feature_report_request_queue.clear();
        self.feature_report_requests_pending.clear();
        self.feature_report_requests_failed.clear();
    }

    fn set_max_pending_input_reports(&mut self, max: usize) {
        self.max_pending_input_reports = max.max(1);
        while self.pending_input_reports.len() > self.max_pending_input_reports {
            self.pending_input_reports.pop_front();
        }
    }

    fn set_max_pending_output_reports(&mut self, max: usize) {
        self.max_pending_output_reports = max.max(1);
        while self.pending_output_reports.len() > self.max_pending_output_reports {
            self.pending_output_reports.pop_front();
        }
    }

    fn report_length(&self, report_type: u8, report_id: u8) -> Option<usize> {
        match report_type {
            1 => self.input_report_lengths.get(&report_id).copied(),
            2 => self.output_report_lengths.get(&report_id).copied(),
            3 => self.feature_report_lengths.get(&report_id).copied(),
            _ => None,
        }
    }

    /// Normalizes a guest-provided Output/Feature report payload (without the report ID prefix)
    /// to a descriptor-derived length.
    ///
    /// For known report IDs this guarantees the returned `Vec<u8>` has the exact payload length
    /// expected by the descriptor (truncating or zero-padding as required). For unknown report IDs
    /// the payload is capped to [`MAX_HID_SET_REPORT_BYTES`] to prevent unbounded allocations.
    fn normalize_report_payload(&self, report_type: u8, report_id: u8, data: &[u8]) -> Vec<u8> {
        let Some(expected_total_len) = self.report_length(report_type, report_id) else {
            // Unknown report ID: cap allocations. Some guests include the report ID prefix even
            // though it is already provided in `wValue`; strip it when it matches.
            let payload = if report_id != 0 && data.first().copied() == Some(report_id) {
                data.get(1..).unwrap_or_default()
            } else {
                data
            };
            let capped = payload.len().min(MAX_HID_SET_REPORT_BYTES);
            return payload[..capped].to_vec();
        };

        let expected_payload_len =
            expected_total_len.saturating_sub(usize::from(report_id != 0));
        if expected_payload_len == 0 {
            return Vec::new();
        }

        // Guests may send either the payload bytes alone (report ID already specified in wValue),
        // or the full report including a report ID prefix. Prefer stripping the prefix only when
        // the transfer is at least as large as the descriptor-defined report length, which avoids
        // discarding a legitimate first payload byte for short transfers.
        let payload = if report_id != 0
            && data.len() >= expected_total_len
            && data.first().copied() == Some(report_id)
        {
            data.get(1..).unwrap_or_default()
        } else {
            data
        };

        let mut out = vec![0u8; expected_payload_len];
        let copy_len = payload.len().min(expected_payload_len);
        out[..copy_len].copy_from_slice(&payload[..copy_len]);
        out
    }

    /// Normalizes a report payload that is known to *not* include a report ID prefix.
    ///
    /// This is primarily used for interrupt OUT transfers where the report ID (if any) has already
    /// been removed from the packet before normalization.
    fn normalize_report_payload_no_prefix(
        &self,
        report_type: u8,
        report_id: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let Some(expected_total_len) = self.report_length(report_type, report_id) else {
            let capped = payload.len().min(MAX_HID_SET_REPORT_BYTES);
            return payload[..capped].to_vec();
        };

        let expected_payload_len =
            expected_total_len.saturating_sub(usize::from(report_id != 0));
        if expected_payload_len == 0 {
            return Vec::new();
        }

        let mut out = vec![0u8; expected_payload_len];
        let copy_len = payload.len().min(expected_payload_len);
        out[..copy_len].copy_from_slice(&payload[..copy_len]);
        out
    }

    fn default_report(&self, report_type: u8, report_id: u8, w_length: u16) -> Vec<u8> {
        let requested = w_length as usize;
        let expected = self
            .report_length(report_type, report_id)
            .unwrap_or(requested);
        let len = expected.min(requested);
        if len == 0 {
            return Vec::new();
        }

        let mut data = vec![0u8; len];
        if report_id != 0 {
            data[0] = report_id;
        }
        data
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
        self.last_output_reports.clear();
        self.last_feature_reports.clear();
        self.cached_feature_reports.clear();
        self.next_feature_report_request_id = 1;
        self.feature_report_request_queue.clear();
        self.feature_report_requests_pending.clear();
        self.feature_report_requests_failed.clear();
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
                        USB_DESCRIPTOR_TYPE_DEVICE => {
                            Some(self.device_descriptor.as_ref().to_vec())
                        }
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => {
                            Some(self.config_descriptor.as_ref().to_vec())
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
                    let prev = self.configuration;
                    self.configuration = config;
                    if self.configuration == 0 {
                        self.pending_input_reports.clear();
                        self.pending_output_reports.clear();
                        self.last_input_reports.clear();
                        self.last_output_reports.clear();
                        self.last_feature_reports.clear();
                    } else if prev == 0 {
                        // Drop any reports that may have been persisted/restored while
                        // unconfigured, then seed the current state from `last_input_reports`.
                        self.pending_input_reports.clear();
                        self.seed_input_reports_on_configuration();
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
                    if setup.request_direction() != RequestDirection::DeviceToHost {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value == 0 && setup.w_index == 0 {
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
                        USB_DESCRIPTOR_TYPE_HID_REPORT => {
                            Some(self.hid_report_descriptor.as_ref().to_vec())
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
                            .unwrap_or_else(|| {
                                self.default_report(report_type, report_id, setup.w_length)
                            }),
                        2 => self
                            .last_output_reports
                            .get(&report_id)
                            .cloned()
                            .unwrap_or_else(|| {
                                self.default_report(report_type, report_id, setup.w_length)
                            }),
                        3 => {
                            if let Some(data) = self.cached_feature_reports.get(&report_id).cloned() {
                                data
                            } else {
                                // If the host previously failed this request, surface the failure
                                // as a timeout-style error (UHCI TD timeout/CRC) so the guest can
                                // retry or recover.
                                if let Some(&pending_id) =
                                    self.feature_report_requests_pending.get(&report_id)
                                {
                                    if self
                                        .feature_report_requests_failed
                                        .get(&report_id)
                                        .is_some_and(|&failed_id| failed_id == pending_id)
                                    {
                                        self.feature_report_requests_pending.remove(&report_id);
                                        self.feature_report_requests_failed.remove(&report_id);
                                        return ControlResponse::Timeout;
                                    }
                                    return ControlResponse::Nak;
                                }

                                self.enqueue_feature_report_request(report_id);
                                return ControlResponse::Nak;
                            }
                        }
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
                            if self.pending_output_reports.len() >= self.max_pending_output_reports {
                                // Backpressure: NAK the STATUS stage until the host drains queued
                                // output/feature reports.
                                return ControlResponse::Nak;
                            }
                            let payload = self.normalize_report_payload(report_type, report_id, data);
                            // Idempotence: only enqueue when returning ACK. If we returned NAK
                            // above, the control pipe will retry the STATUS stage without having
                            // enqueued anything yet.
                            let pushed = self.try_push_output_report(UsbHidPassthroughOutputReport {
                                report_type,
                                report_id,
                                data: payload,
                            });
                            debug_assert!(pushed);
                            if !pushed {
                                return ControlResponse::Nak;
                            }
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
        match self.pending_input_reports.pop_front() {
            Some(data) => UsbInResult::Data(data),
            None => UsbInResult::Nak,
        }
    }

    fn handle_interrupt_out(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        if ep != INTERRUPT_OUT_EP || !self.has_interrupt_out {
            return UsbOutResult::Stall;
        }
        if self.configuration == 0 || self.interrupt_out_halted {
            return UsbOutResult::Stall;
        }
        if self.pending_output_reports.len() >= self.max_pending_output_reports {
            // Backpressure: NAK the OUT transaction until the host drains queued output reports.
            // Do not mutate `last_*_reports` so GET_REPORT continues to reflect the last accepted
            // report rather than a report that was never delivered to the host.
            return UsbOutResult::Nak;
        }

        let (report_id, payload) = if self.report_ids_in_use {
            if data.is_empty() {
                (0, &[][..])
            } else {
                (data[0], &data[1..])
            }
        } else {
            (0, data)
        };
        let payload = self.normalize_report_payload_no_prefix(2, report_id, payload);

        let pushed = self.try_push_output_report(UsbHidPassthroughOutputReport {
            report_type: 2, // Output
            report_id,
            data: payload,
        });
        debug_assert!(pushed);
        if pushed {
            UsbOutResult::Ack
        } else {
            UsbOutResult::Nak
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
        USB_DESCRIPTOR_TYPE_DEVICE,
        0x00,
        0x02,             // bcdUSB (2.00)
        0x00,             // bDeviceClass (per interface)
        0x00,             // bDeviceSubClass
        0x00,             // bDeviceProtocol
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
    let total_len =
        9u16 + 9u16 + hid_descriptor.len() as u16 + 7u16 + if has_interrupt_out { 7 } else { 0 };
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
        0x00,          // bInterfaceNumber
        0x00,          // bAlternateSetting
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

fn report_descriptor_report_lengths(
    report_descriptor_bytes: &[u8],
) -> ReportDescriptorReportLengths {
    let Ok(parsed) = report_descriptor::parse_report_descriptor(report_descriptor_bytes) else {
        let (report_ids_in_use, input_bits, output_bits, feature_bits) =
            scan_report_descriptor_bits(report_descriptor_bytes);
        return (
            report_ids_in_use,
            bits_to_report_lengths(&input_bits),
            bits_to_report_lengths(&output_bits),
            bits_to_report_lengths(&feature_bits),
        );
    };

    let mut report_ids_in_use = false;
    let mut input_bits: BTreeMap<u8, u64> = BTreeMap::new();
    let mut output_bits: BTreeMap<u8, u64> = BTreeMap::new();
    let mut feature_bits: BTreeMap<u8, u64> = BTreeMap::new();

    for collection in &parsed {
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

type ReportDescriptorReportLengths = (
    bool,
    BTreeMap<u8, usize>,
    BTreeMap<u8, usize>,
    BTreeMap<u8, usize>,
);
fn bits_to_report_lengths(bits: &BTreeMap<u8, u64>) -> BTreeMap<u8, usize> {
    let mut out = BTreeMap::new();
    for (&report_id, &total_bits) in bits {
        let mut bytes = usize::try_from(total_bits.saturating_add(7) / 8).unwrap_or(usize::MAX);
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
    map.entry(report_id)
        .and_modify(|v| *v = v.saturating_add(bits))
        .or_insert(bits);
}

fn report_bits(report: &report_descriptor::HidReportInfo) -> u64 {
    report
        .items
        .iter()
        .map(|item| u64::from(item.report_size).saturating_mul(u64::from(item.report_count)))
        .fold(0u64, |acc, v| acc.saturating_add(v))
}

#[derive(Debug, Clone, Copy, Default)]
struct ScanGlobalState {
    report_id: u32,
    report_size: u32,
    report_count: u32,
}

fn scan_parse_unsigned(data: &[u8]) -> u32 {
    match data.len() {
        0 => 0,
        1 => data[0] as u32,
        2 => u16::from_le_bytes([data[0], data[1]]) as u32,
        4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    }
}

type ScanReportDescriptorBits = (
    bool,
    BTreeMap<u8, u64>,
    BTreeMap<u8, u64>,
    BTreeMap<u8, u64>,
);

fn scan_report_descriptor_bits(report_descriptor: &[u8]) -> ScanReportDescriptorBits {
    let mut global = ScanGlobalState::default();
    let mut global_stack: Vec<ScanGlobalState> = Vec::new();

    let mut report_ids_in_use = false;
    let mut input_bits: BTreeMap<u8, u64> = BTreeMap::new();
    let mut output_bits: BTreeMap<u8, u64> = BTreeMap::new();
    let mut feature_bits: BTreeMap<u8, u64> = BTreeMap::new();

    let mut i = 0usize;
    while i < report_descriptor.len() {
        let prefix = report_descriptor[i];
        i += 1;

        if prefix == 0xFE {
            // Long item: bSize, bTag, data...
            if i + 2 > report_descriptor.len() {
                break;
            }
            let size = report_descriptor[i] as usize;
            i += 2;
            i = i.saturating_add(size);
            continue;
        }

        let size = match prefix & 0x03 {
            0 => 0usize,
            1 => 1usize,
            2 => 2usize,
            3 => 4usize,
            _ => 0usize,
        };

        if i + size > report_descriptor.len() {
            break;
        }

        let item_type = (prefix >> 2) & 0x03;
        let tag = (prefix >> 4) & 0x0F;

        let data = &report_descriptor[i..i + size];
        i += size;

        match (item_type, tag) {
            // Global items.
            (1, 7) => global.report_size = scan_parse_unsigned(data),
            (1, 9) => global.report_count = scan_parse_unsigned(data),
            (1, 8) => {
                global.report_id = scan_parse_unsigned(data);
                if global.report_id != 0 {
                    report_ids_in_use = true;
                }
            }
            (1, 10) => {
                // Push
                if data.is_empty() {
                    global_stack.push(global);
                }
            }
            (1, 11) => {
                // Pop
                if data.is_empty() {
                    if let Some(prev) = global_stack.pop() {
                        global = prev;
                    }
                }
            }
            // Main items: Input / Output / Feature.
            (0, 8) | (0, 9) | (0, 11) => {
                let Ok(report_id) = u8::try_from(global.report_id) else {
                    report_ids_in_use = true;
                    continue;
                };
                let bits =
                    u64::from(global.report_size).saturating_mul(u64::from(global.report_count));
                match tag {
                    8 => add_bits(&mut input_bits, report_id, bits),
                    9 => add_bits(&mut output_bits, report_id, bits),
                    11 => add_bits(&mut feature_bits, report_id, bits),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    (report_ids_in_use, input_bits, output_bits, feature_bits)
}

fn scan_relative_input_bit_ranges(report_descriptor: &[u8]) -> BTreeMap<u8, Vec<(u64, u64)>> {
    let mut global = ScanGlobalState::default();
    let mut global_stack: Vec<ScanGlobalState> = Vec::new();

    let mut offsets: BTreeMap<u8, u64> = BTreeMap::new();
    let mut out: BTreeMap<u8, Vec<(u64, u64)>> = BTreeMap::new();

    let mut i = 0usize;
    while i < report_descriptor.len() {
        let prefix = report_descriptor[i];
        i += 1;

        if prefix == 0xFE {
            // Long item: bSize, bTag, data...
            if i + 2 > report_descriptor.len() {
                break;
            }
            let size = report_descriptor[i] as usize;
            i += 2;
            i = i.saturating_add(size);
            continue;
        }

        let size = match prefix & 0x03 {
            0 => 0usize,
            1 => 1usize,
            2 => 2usize,
            3 => 4usize,
            _ => 0usize,
        };

        if i + size > report_descriptor.len() {
            break;
        }

        let item_type = (prefix >> 2) & 0x03;
        let tag = (prefix >> 4) & 0x0F;

        let data = &report_descriptor[i..i + size];
        i += size;

        match (item_type, tag) {
            // Global items.
            (1, 7) => global.report_size = scan_parse_unsigned(data),
            (1, 9) => global.report_count = scan_parse_unsigned(data),
            (1, 8) => global.report_id = scan_parse_unsigned(data),
            (1, 10) => {
                // Push
                if data.is_empty() {
                    global_stack.push(global);
                }
            }
            (1, 11) => {
                // Pop
                if data.is_empty() {
                    if let Some(prev) = global_stack.pop() {
                        global = prev;
                    }
                }
            }
            // Main item: Input
            (0, 8) => {
                let Ok(report_id) = u8::try_from(global.report_id) else {
                    continue;
                };
                let bits =
                    u64::from(global.report_size).saturating_mul(u64::from(global.report_count));
                let start = offsets.get(&report_id).copied().unwrap_or(0);
                offsets.insert(report_id, start.saturating_add(bits));

                // Input main item flags: bit 2 is Relative(1)/Absolute(0).
                let relative = !data.is_empty() && (data[0] & 0x04) != 0;
                if relative {
                    out.entry(report_id).or_default().push((start, bits));
                }
            }
            _ => {}
        }
    }

    out
}

fn clear_bits(bytes: &mut [u8], start_bit: usize, len_bits: usize) {
    if len_bits == 0 {
        return;
    }

    let total_bits = bytes.len().saturating_mul(8);
    if start_bit >= total_bits {
        return;
    }

    let end_bit = start_bit.saturating_add(len_bits).min(total_bits);
    for bit in start_bit..end_bit {
        let byte = bit / 8;
        let bit_in_byte = bit % 8;
        bytes[byte] &= !(1u8 << bit_in_byte);
    }
}

fn encode_report_map(map: &BTreeMap<u8, Vec<u8>>) -> Vec<u8> {
    let mut enc = Encoder::new().u32(map.len() as u32);
    for (&report_id, data) in map {
        enc = enc.u8(report_id).u32(data.len() as u32).bytes(data);
    }
    enc.finish()
}

fn decode_report_map(
    map: &mut BTreeMap<u8, Vec<u8>>,
    buf: &[u8],
    what: &'static str,
) -> SnapshotResult<()> {
    const MAX_REPORTS: usize = 1024;
    const MAX_REPORT_BYTES: usize = 1024 * 1024;

    let mut d = Decoder::new(buf);
    let count = d.u32()? as usize;
    if count > MAX_REPORTS {
        return Err(SnapshotError::InvalidFieldEncoding(what));
    }

    map.clear();
    for _ in 0..count {
        let report_id = d.u8()?;
        let len = d.u32()? as usize;
        if len > MAX_REPORT_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding(what));
        }
        let data = d.bytes(len)?.to_vec();
        map.insert(report_id, data);
    }
    d.finish()?;
    Ok(())
}

impl IoSnapshot for UsbHidPassthrough {
    const DEVICE_ID: [u8; 4] = *b"HIDP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 3);

    fn save_state(&self) -> Vec<u8> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_CONFIGURATION: u16 = 2;
        const TAG_REMOTE_WAKEUP: u16 = 3;
        const TAG_INTERRUPT_IN_HALTED: u16 = 4;
        const TAG_INTERRUPT_OUT_HALTED: u16 = 5;
        const TAG_PROTOCOL: u16 = 6;
        const TAG_IDLE_RATE: u16 = 7;
        const TAG_MAX_PENDING_INPUT_REPORTS: u16 = 8;
        const TAG_MAX_PENDING_OUTPUT_REPORTS: u16 = 9;
        const TAG_PENDING_INPUT_REPORTS: u16 = 10;
        const TAG_LAST_INPUT_REPORTS: u16 = 11;
        const TAG_LAST_OUTPUT_REPORTS: u16 = 12;
        const TAG_LAST_FEATURE_REPORTS: u16 = 13;
        const TAG_PENDING_OUTPUT_REPORTS: u16 = 14;
        const TAG_CACHED_FEATURE_REPORTS: u16 = 25;
        const TAG_NEXT_FEATURE_REPORT_REQUEST_ID: u16 = 26;
        const TAG_FEATURE_REPORT_REQUEST_QUEUE: u16 = 27;
        const TAG_FEATURE_REPORT_REQUESTS_PENDING: u16 = 28;
        const TAG_FEATURE_REPORT_REQUESTS_FAILED: u16 = 29;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u8(TAG_ADDRESS, self.address);
        w.field_u8(TAG_CONFIGURATION, self.configuration);
        w.field_bool(TAG_REMOTE_WAKEUP, self.remote_wakeup_enabled);
        w.field_bool(TAG_INTERRUPT_IN_HALTED, self.interrupt_in_halted);
        w.field_bool(TAG_INTERRUPT_OUT_HALTED, self.interrupt_out_halted);
        w.field_u8(TAG_PROTOCOL, self.protocol as u8);
        w.field_u8(TAG_IDLE_RATE, self.idle_rate);
        w.field_u32(
            TAG_MAX_PENDING_INPUT_REPORTS,
            self.max_pending_input_reports as u32,
        );
        w.field_u32(
            TAG_MAX_PENDING_OUTPUT_REPORTS,
            self.max_pending_output_reports as u32,
        );

        let pending: Vec<Vec<u8>> = self.pending_input_reports.iter().cloned().collect();
        w.field_bytes(
            TAG_PENDING_INPUT_REPORTS,
            Encoder::new().vec_bytes(&pending).finish(),
        );
        w.field_bytes(
            TAG_LAST_INPUT_REPORTS,
            encode_report_map(&self.last_input_reports),
        );
        w.field_bytes(
            TAG_LAST_OUTPUT_REPORTS,
            encode_report_map(&self.last_output_reports),
        );
        w.field_bytes(
            TAG_LAST_FEATURE_REPORTS,
            encode_report_map(&self.last_feature_reports),
        );
        w.field_bytes(
            TAG_CACHED_FEATURE_REPORTS,
            encode_report_map(&self.cached_feature_reports),
        );

        let mut pending_out = Encoder::new().u32(self.pending_output_reports.len() as u32);
        for report in &self.pending_output_reports {
            pending_out = pending_out
                .u8(report.report_type)
                .u8(report.report_id)
                .u32(report.data.len() as u32)
                .bytes(&report.data);
        }
        w.field_bytes(TAG_PENDING_OUTPUT_REPORTS, pending_out.finish());

        w.field_u32(
            TAG_NEXT_FEATURE_REPORT_REQUEST_ID,
            self.next_feature_report_request_id,
        );

        let mut feature_queue =
            Encoder::new().u32(self.feature_report_request_queue.len() as u32);
        for req in &self.feature_report_request_queue {
            feature_queue = feature_queue.u32(req.request_id).u8(req.report_id);
        }
        w.field_bytes(TAG_FEATURE_REPORT_REQUEST_QUEUE, feature_queue.finish());

        let mut feature_pending =
            Encoder::new().u32(self.feature_report_requests_pending.len() as u32);
        for (&report_id, &request_id) in &self.feature_report_requests_pending {
            feature_pending = feature_pending.u8(report_id).u32(request_id);
        }
        w.field_bytes(TAG_FEATURE_REPORT_REQUESTS_PENDING, feature_pending.finish());

        let mut feature_failed =
            Encoder::new().u32(self.feature_report_requests_failed.len() as u32);
        for (&report_id, &request_id) in &self.feature_report_requests_failed {
            feature_failed = feature_failed.u8(report_id).u32(request_id);
        }
        w.field_bytes(TAG_FEATURE_REPORT_REQUESTS_FAILED, feature_failed.finish());

        // Static metadata required for reconstruction.
        if let Some((vendor_id, product_id, max_packet_size)) =
            parse_device_descriptor_fields(self.device_descriptor.as_ref())
        {
            w.field_u16(HIDP_SNAP_TAG_VENDOR_ID, vendor_id);
            w.field_u16(HIDP_SNAP_TAG_PRODUCT_ID, product_id);
            w.field_u16(HIDP_SNAP_TAG_MAX_PACKET_SIZE, max_packet_size);
        }

        if let Some((subclass, protocol)) =
            parse_interface_descriptor_fields(self.config_descriptor.as_ref())
        {
            w.field_u8(HIDP_SNAP_TAG_INTERFACE_SUBCLASS, subclass);
            w.field_u8(HIDP_SNAP_TAG_INTERFACE_PROTOCOL, protocol);
        }

        if let Some(s) =
            decode_string_descriptor_utf16le(self.manufacturer_string_descriptor.as_ref())
        {
            w.field_bytes(HIDP_SNAP_TAG_MANUFACTURER, s.into_bytes());
        }
        if let Some(s) = decode_string_descriptor_utf16le(self.product_string_descriptor.as_ref()) {
            w.field_bytes(HIDP_SNAP_TAG_PRODUCT, s.into_bytes());
        }
        if let Some(desc) = self.serial_string_descriptor.as_ref() {
            if let Some(s) = decode_string_descriptor_utf16le(desc.as_ref()) {
                w.field_bytes(HIDP_SNAP_TAG_SERIAL, s.into_bytes());
            }
        }

        w.field_bytes(
            HIDP_SNAP_TAG_HID_REPORT_DESCRIPTOR,
            self.hid_report_descriptor.as_ref().to_vec(),
        );
        w.field_bool(HIDP_SNAP_TAG_HAS_INTERRUPT_OUT, self.has_interrupt_out);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_CONFIGURATION: u16 = 2;
        const TAG_REMOTE_WAKEUP: u16 = 3;
        const TAG_INTERRUPT_IN_HALTED: u16 = 4;
        const TAG_INTERRUPT_OUT_HALTED: u16 = 5;
        const TAG_PROTOCOL: u16 = 6;
        const TAG_IDLE_RATE: u16 = 7;
        const TAG_MAX_PENDING_INPUT_REPORTS: u16 = 8;
        const TAG_MAX_PENDING_OUTPUT_REPORTS: u16 = 9;
        const TAG_PENDING_INPUT_REPORTS: u16 = 10;
        const TAG_LAST_INPUT_REPORTS: u16 = 11;
        const TAG_LAST_OUTPUT_REPORTS: u16 = 12;
        const TAG_LAST_FEATURE_REPORTS: u16 = 13;
        const TAG_PENDING_OUTPUT_REPORTS: u16 = 14;
        const TAG_CACHED_FEATURE_REPORTS: u16 = 25;
        const TAG_NEXT_FEATURE_REPORT_REQUEST_ID: u16 = 26;
        const TAG_FEATURE_REPORT_REQUEST_QUEUE: u16 = 27;
        const TAG_FEATURE_REPORT_REQUESTS_PENDING: u16 = 28;
        const TAG_FEATURE_REPORT_REQUESTS_FAILED: u16 = 29;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Reset guest-visible state while preserving static descriptor/report metadata.
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
        self.last_output_reports.clear();
        self.last_feature_reports.clear();
        self.cached_feature_reports.clear();
        self.next_feature_report_request_id = 1;
        self.feature_report_request_queue.clear();
        self.feature_report_requests_pending.clear();
        self.feature_report_requests_failed.clear();

        self.address = r.u8(TAG_ADDRESS)?.unwrap_or(0);
        self.configuration = r.u8(TAG_CONFIGURATION)?.unwrap_or(0);
        self.remote_wakeup_enabled = r.bool(TAG_REMOTE_WAKEUP)?.unwrap_or(false);
        self.interrupt_in_halted = r.bool(TAG_INTERRUPT_IN_HALTED)?.unwrap_or(false);
        self.interrupt_out_halted = r.bool(TAG_INTERRUPT_OUT_HALTED)?.unwrap_or(false);

        if let Some(protocol) = r.u8(TAG_PROTOCOL)? {
            self.protocol = match protocol {
                0 => HidProtocol::Boot,
                1 => HidProtocol::Report,
                _ => return Err(SnapshotError::InvalidFieldEncoding("hid protocol")),
            };
        }

        self.idle_rate = r.u8(TAG_IDLE_RATE)?.unwrap_or(0);

        if let Some(max) = r.u32(TAG_MAX_PENDING_INPUT_REPORTS)? {
            self.max_pending_input_reports = (max as usize).max(1);
        }
        if let Some(max) = r.u32(TAG_MAX_PENDING_OUTPUT_REPORTS)? {
            self.max_pending_output_reports = (max as usize).max(1);
        }

        if let Some(buf) = r.bytes(TAG_PENDING_INPUT_REPORTS) {
            let mut d = Decoder::new(buf);
            let reports = d.vec_bytes()?;
            d.finish()?;
            if reports.len() > self.max_pending_input_reports {
                return Err(SnapshotError::InvalidFieldEncoding("pending input reports"));
            }
            self.pending_input_reports = reports.into_iter().collect();
        }

        if let Some(buf) = r.bytes(TAG_PENDING_OUTPUT_REPORTS) {
            const MAX_REPORT_BYTES: usize = 128 * 1024;

            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > self.max_pending_output_reports {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "pending output reports",
                ));
            }
            for _ in 0..count {
                let report_type = d.u8()?;
                let report_id = d.u8()?;
                let len = d.u32()? as usize;
                if len > MAX_REPORT_BYTES {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "pending output reports",
                    ));
                }
                let data = d.bytes(len)?.to_vec();
                self.pending_output_reports
                    .push_back(UsbHidPassthroughOutputReport {
                        report_type,
                        report_id,
                        data,
                    });
            }
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_LAST_INPUT_REPORTS) {
            decode_report_map(&mut self.last_input_reports, buf, "last input reports")?;
        }
        if let Some(buf) = r.bytes(TAG_LAST_OUTPUT_REPORTS) {
            decode_report_map(&mut self.last_output_reports, buf, "last output reports")?;
        }
        if let Some(buf) = r.bytes(TAG_LAST_FEATURE_REPORTS) {
            decode_report_map(&mut self.last_feature_reports, buf, "last feature reports")?;
        }
        if let Some(buf) = r.bytes(TAG_CACHED_FEATURE_REPORTS) {
            decode_report_map(
                &mut self.cached_feature_reports,
                buf,
                "cached feature reports",
            )?;
        }

        self.next_feature_report_request_id = r
            .u32(TAG_NEXT_FEATURE_REPORT_REQUEST_ID)?
            .unwrap_or(1)
            .max(1);

        if let Some(buf) = r.bytes(TAG_FEATURE_REPORT_REQUEST_QUEUE) {
            const MAX_PENDING: usize = 1024;

            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_PENDING {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "feature report request queue",
                ));
            }
            for _ in 0..count {
                let request_id = d.u32()?;
                let report_id = d.u8()?;
                self.feature_report_request_queue
                    .push_back(UsbHidPassthroughFeatureReportRequest {
                        request_id,
                        report_id,
                    });
            }
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_FEATURE_REPORT_REQUESTS_PENDING) {
            const MAX_PENDING: usize = 1024;

            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_PENDING {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "feature report requests pending",
                ));
            }
            for _ in 0..count {
                let report_id = d.u8()?;
                let request_id = d.u32()?;
                self.feature_report_requests_pending
                    .insert(report_id, request_id);
            }
            d.finish()?;
        } else {
            // Backwards-compatible: older snapshots may only store the queue. Derive the pending
            // set from queued requests so repeated GET_REPORT polls won't enqueue duplicates.
            for req in &self.feature_report_request_queue {
                self.feature_report_requests_pending
                    .insert(req.report_id, req.request_id);
            }
        }

        if let Some(buf) = r.bytes(TAG_FEATURE_REPORT_REQUESTS_FAILED) {
            const MAX_PENDING: usize = 1024;

            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_PENDING {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "feature report requests failed",
                ));
            }
            for _ in 0..count {
                let report_id = d.u8()?;
                let request_id = d.u32()?;
                self.feature_report_requests_failed.insert(report_id, request_id);
            }
            d.finish()?;
        }

        // `pop_feature_report_request` drains the host request queue destructively. If a snapshot is
        // taken after the host has popped a feature report request (but before completing it), the
        // queue may be empty while `feature_report_requests_pending` still contains the in-flight
        // request. If we restore that state as-is, the guest will keep NAKing forever because the
        // host runtime no longer has a way to rediscover the request.
        //
        // Make snapshot restore deterministic by re-queuing any in-flight requests so the host can
        // service them again after restore.
        for req in &self.feature_report_request_queue {
            self.feature_report_requests_pending
                .entry(req.report_id)
                .or_insert(req.request_id);
        }
        self.feature_report_requests_failed
            .retain(|report_id, request_id| {
                self.feature_report_requests_pending.get(report_id) == Some(request_id)
            });

        self.feature_report_request_queue.retain(|req| {
            self.feature_report_requests_pending.get(&req.report_id) == Some(&req.request_id)
        });

        let mut queued = BTreeSet::<u32>::new();
        for req in &self.feature_report_request_queue {
            queued.insert(req.request_id);
        }
        for (&report_id, &request_id) in &self.feature_report_requests_pending {
            if queued.contains(&request_id) {
                continue;
            }
            self.feature_report_request_queue
                .push_back(UsbHidPassthroughFeatureReportRequest {
                    request_id,
                    report_id,
                });
        }

        Ok(())
    }
}

impl IoSnapshot for UsbHidPassthroughHandle {
    const DEVICE_ID: [u8; 4] = UsbHidPassthrough::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = UsbHidPassthrough::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.inner.borrow().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        self.inner.borrow_mut().load_state(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::AttachedUsbDevice;

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

    fn sample_report_descriptor_with_unsupported_item() -> Vec<u8> {
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
            0x69, 0x01, // Reserved local item (unsupported by parser)
            0x81, 0x02, // Input (Data,Var,Abs)
            0xc0, // End Collection
        ]
    }

    fn sample_mouse_report_descriptor_relative_xy() -> Vec<u8> {
        vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x02, // Usage (Mouse)
            0xa1, 0x01, // Collection (Application)
            0x85, 0x01, // Report ID (1)
            0x09, 0x01, // Usage (Pointer)
            0xa1, 0x00, // Collection (Physical)
            0x05, 0x09, // Usage Page (Buttons)
            0x19, 0x01, // Usage Minimum (Button 1)
            0x29, 0x03, // Usage Maximum (Button 3)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x01, // Logical Maximum (1)
            0x95, 0x03, // Report Count (3)
            0x75, 0x01, // Report Size (1)
            0x81, 0x02, // Input (Data,Var,Abs)
            0x95, 0x01, // Report Count (1)
            0x75, 0x05, // Report Size (5)
            0x81, 0x01, // Input (Const,Array,Abs) padding
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x30, // Usage (X)
            0x09, 0x31, // Usage (Y)
            0x15, 0x81, // Logical Minimum (-127)
            0x25, 0x7f, // Logical Maximum (127)
            0x75, 0x08, // Report Size (8)
            0x95, 0x02, // Report Count (2)
            0x81, 0x06, // Input (Data,Var,Rel)
            0xc0, // End Collection
            0xc0, // End Collection
        ]
    }

    fn sample_report_descriptor_output_with_id() -> Vec<u8> {
        vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x00, // Usage (Undefined)
            0xa1, 0x01, // Collection (Application)
            0x85, 0x02, // Report ID (2)
            0x09, 0x00, // Usage (Undefined)
            0x15, 0x00, // Logical Minimum (0)
            0x26, 0xff, 0x00, // Logical Maximum (255)
            0x75, 0x08, // Report Size (8)
            0x95, 0x02, // Report Count (2)
            0x91, 0x02, // Output (Data,Var,Abs)
            0xc0, // End Collection
        ]
    }

    fn sample_report_descriptor_feature_with_id() -> Vec<u8> {
        vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x00, // Usage (Undefined)
            0xa1, 0x01, // Collection (Application)
            0x85, 0x05, // Report ID (5)
            0x09, 0x00, // Usage (Undefined)
            0x15, 0x00, // Logical Minimum (0)
            0x26, 0xff, 0x00, // Logical Maximum (255)
            0x75, 0x08, // Report Size (8)
            0x95, 0x04, // Report Count (4)
            0xb1, 0x02, // Feature (Data,Var,Abs)
            0xc0, // End Collection
        ]
    }

    #[test]
    fn descriptors_are_well_formed() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            Some("Serial".into()),
            report.clone(),
            true,
            None,
            Some(1),
            Some(1),
        );

        let device_desc = match dev.handle_control_request(
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
        assert_eq!(device_desc.len(), 18);
        assert_eq!(device_desc[0] as usize, device_desc.len());
        assert_eq!(device_desc[1], USB_DESCRIPTOR_TYPE_DEVICE);

        let cfg = match dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: USB_REQUEST_GET_DESCRIPTOR,
                w_value: (USB_DESCRIPTOR_TYPE_CONFIGURATION as u16) << 8,
                w_index: 0,
                w_length: 255,
            },
            None,
        ) {
            ControlResponse::Data(data) => data,
            other => panic!("expected Data response, got {other:?}"),
        };
        assert_eq!(cfg[0], 0x09);
        assert_eq!(cfg[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(w_le(&cfg, 2) as usize, cfg.len());

        let (subclass, protocol) =
            parse_interface_descriptor_fields(&cfg).expect("config descriptor should contain interface descriptor");
        assert_eq!(subclass, 1, "interface subclass should match constructor parameter");
        assert_eq!(protocol, 1, "interface protocol should match constructor parameter");

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
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        dev.push_input_report(1, &[0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Nak
        );

        configure_device(&mut dev);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0xaa, 0xbb, 0xcc, 0xdd])
        );

        dev.push_input_report(0, &[0x11, 0x22]);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![0x11, 0x22])
        );
    }

    #[test]
    fn push_input_report_pads_short_input_report_to_descriptor_length() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // Descriptor defines report ID 1 with 4 bytes of payload (5 bytes total including ID).
        dev.push_input_report(1, &[0xaa, 0xbb]);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0xaa, 0xbb, 0, 0])
        );

        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (1u16 << 8) | 1u16, // Input, report ID 1
                w_index: 0,
                w_length: 64,
            },
            None,
        );
        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data, vec![1, 0xaa, 0xbb, 0, 0]);
    }

    #[test]
    fn push_input_report_truncates_long_input_report_to_descriptor_length() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        dev.push_input_report(1, &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0xaa, 0xbb, 0xcc, 0xdd])
        );

        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (1u16 << 8) | 1u16, // Input, report ID 1
                w_index: 0,
                w_length: 64,
            },
            None,
        );
        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data, vec![1, 0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[test]
    fn push_input_report_accepts_already_prefixed_input_report() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // Pass bytes already prefixed with the report ID; the queued report should not be double
        // prefixed.
        dev.push_input_report(1, &[1, 0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0xaa, 0xbb, 0xcc, 0xdd])
        );
    }

    #[test]
    fn push_input_report_unknown_report_id_is_capped() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        configure_device(&mut dev);

        let report_id = 0x99;
        let big_payload = vec![0xaa; u16::MAX as usize];
        dev.push_input_report(report_id, &big_payload);

        let UsbInResult::Data(data) = dev.handle_in_transfer(INTERRUPT_IN_EP, 1024) else {
            panic!("expected data");
        };
        assert_eq!(data.len(), super::MAX_UNKNOWN_INPUT_REPORT_BYTES);
        assert_eq!(data[0], report_id);
        assert_eq!(&data[1..], &[0xaa; super::MAX_UNKNOWN_INPUT_REPORT_BYTES - 1]);
    }

    #[test]
    fn configuration_seeds_last_report_but_clears_relative_axes() {
        let report = sample_mouse_report_descriptor_relative_xy();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        // Buttons + relative motion before configuration.
        dev.push_input_report(1, &[0x01, 0x05, 0xfb]);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Nak
        );

        configure_device(&mut dev);

        // Seed report should preserve the held button bit, but clear relative X/Y deltas.
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0x01, 0x00, 0x00])
        );
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Nak
        );

        // After configuration, relative motion should be delivered unchanged.
        dev.push_input_report(1, &[0x01, 0x05, 0xfb]);
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0x01, 0x05, 0xfb])
        );
    }

    #[test]
    fn configuration_does_not_seed_pure_relative_motion() {
        let report = sample_mouse_report_descriptor_relative_xy();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        dev.push_input_report(1, &[0x00, 0x10, 0xf0]);

        configure_device(&mut dev);

        // Relative motion is cleared to zero; with no buttons held the seeded report is default,
        // so nothing should be queued.
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Nak
        );
    }

    #[test]
    fn get_report_returns_zero_filled_report_of_descriptor_length() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        // Descriptor defines report ID 1 with 4 bytes of payload.
        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (1u16 << 8) | 1u16, // Input, report ID 1
                w_index: 0,
                w_length: 64,
            },
            None,
        );

        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data, vec![1, 0, 0, 0, 0]);
    }

    #[test]
    fn get_report_uses_scanner_when_report_descriptor_parser_rejects_descriptor() {
        let report = sample_report_descriptor_with_unsupported_item();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        // Descriptor defines report ID 1 with 4 bytes of payload.
        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (1u16 << 8) | 1u16, // Input, report ID 1
                w_index: 0,
                w_length: 64,
            },
            None,
        );

        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data, vec![1, 0, 0, 0, 0]);
    }

    #[test]
    fn set_report_and_interrupt_out_are_queued() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
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
    fn set_report_strips_report_id_prefix_when_present() {
        let report = sample_report_descriptor_output_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_REPORT,
                    w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                    w_index: 0,
                    w_length: 3,
                },
                Some(&[2, 0x11, 0x22]),
            ),
            ControlResponse::Ack
        );

        assert_eq!(
            dev.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0x11, 0x22],
            })
        );
    }

    #[test]
    fn set_report_truncates_oversized_payload_to_descriptor_length() {
        let report = sample_report_descriptor_output_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        // Descriptor defines report ID 2 with 2 bytes of payload. Send an oversized transfer with
        // a large `wLength`; only the descriptor-sized payload should be queued.
        let mut big = vec![0u8; u16::MAX as usize];
        big[0] = 2; // report ID prefix
        big[1] = 0x11;
        big[2] = 0x22;
        big[3] = 0x33;

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_REPORT,
                    w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                    w_index: 0,
                    w_length: u16::MAX,
                },
                Some(&big),
            ),
            ControlResponse::Ack
        );

        assert_eq!(
            dev.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0x11, 0x22],
            })
        );
    }

    #[test]
    fn set_report_pads_short_payload_to_descriptor_length() {
        let report = sample_report_descriptor_output_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        // Provide only a single payload byte (no report ID prefix). The queued payload should be
        // padded with zeros to the descriptor-defined length (2 bytes).
        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_REPORT,
                    w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                    w_index: 0,
                    w_length: 1,
                },
                Some(&[0x11]),
            ),
            ControlResponse::Ack
        );

        assert_eq!(
            dev.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0x11, 0x00],
            })
        );
    }

    #[test]
    fn set_report_unknown_report_id_is_capped() {
        let report = sample_report_descriptor_output_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        let report_id = 0x99;
        let data = vec![0xaa; u16::MAX as usize];

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_REPORT,
                    w_value: (3u16 << 8) | report_id as u16, // Feature, unknown report ID
                    w_index: 0,
                    w_length: u16::MAX,
                },
                Some(&data),
            ),
            ControlResponse::Ack
        );

        let r = dev.pop_output_report().unwrap();
        assert_eq!(r.report_type, 3);
        assert_eq!(r.report_id, report_id);
        assert_eq!(r.data.len(), super::MAX_HID_SET_REPORT_BYTES);
        assert_eq!(&r.data[..8], &[0xaa; 8]);
    }

    #[test]
    fn interrupt_out_is_normalized_to_descriptor_length() {
        let report = sample_report_descriptor_output_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            true,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // Oversized interrupt OUT: should be truncated.
        assert_eq!(
            dev.handle_interrupt_out(0x01, &[2, 0x11, 0x22, 0x33]),
            UsbOutResult::Ack
        );
        assert_eq!(
            dev.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0x11, 0x22],
            })
        );

        // Short interrupt OUT: should be padded.
        assert_eq!(
            dev.handle_interrupt_out(0x01, &[2, 0x11]),
            UsbOutResult::Ack
        );
        assert_eq!(
            dev.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0x11, 0x00],
            })
        );
    }

    #[test]
    fn get_report_output_returns_last_received_report() {
        let report = sample_report_descriptor_output_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        // Deliver an Output report via SET_REPORT.
        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_REPORT,
                    w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                    w_index: 0,
                    w_length: 3,
                },
                Some(&[2, 0x11, 0x22]),
            ),
            ControlResponse::Ack
        );

        // GET_REPORT should include the report ID prefix for non-zero IDs.
        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                w_index: 0,
                w_length: 64,
            },
            None,
        );
        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data, vec![2, 0x11, 0x22]);
    }

    #[test]
    fn set_max_pending_report_limits_apply_backpressure() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            true,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        dev.set_max_pending_input_reports(2);
        dev.push_input_report(1, &[0x00]);
        dev.push_input_report(1, &[0x01]);
        dev.push_input_report(1, &[0x02]);

        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0x01, 0, 0, 0])
        );
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Data(vec![1, 0x02, 0, 0, 0])
        );
        assert_eq!(
            dev.handle_in_transfer(INTERRUPT_IN_EP, 64),
            UsbInResult::Nak
        );

        dev.set_max_pending_output_reports(1);
        assert_eq!(
            dev.handle_interrupt_out(0x01, &[1, 0x10]),
            UsbOutResult::Ack
        );
        assert_eq!(
            dev.handle_interrupt_out(0x01, &[1, 0x20]),
            UsbOutResult::Nak
        );

        assert_eq!(
            dev.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 1,
                data: vec![0x10]
            })
        );
        assert_eq!(dev.pop_output_report(), None);
    }

    #[test]
    fn interrupt_out_naks_when_output_queue_full_and_preserves_last_report() {
        let report = sample_report_descriptor_output_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            true,
            None,
            None,
            None,
        );
        configure_device(&mut dev);
        dev.set_max_pending_output_reports(1);

        assert_eq!(
            dev.handle_interrupt_out(INTERRUPT_OUT_EP, &[2, 0x10, 0x20]),
            UsbOutResult::Ack
        );
        assert_eq!(dev.inner.borrow().pending_output_reports.len(), 1);

        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                w_index: 0,
                w_length: 64,
            },
            None,
        );
        assert_eq!(resp, ControlResponse::Data(vec![2, 0x10, 0x20]));

        // Queue is full; this should NAK and not clobber `last_output_reports`.
        assert_eq!(
            dev.handle_interrupt_out(INTERRUPT_OUT_EP, &[2, 0xaa, 0xbb]),
            UsbOutResult::Nak
        );
        assert_eq!(dev.inner.borrow().pending_output_reports.len(), 1);

        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                w_index: 0,
                w_length: 64,
            },
            None,
        );
        assert_eq!(resp, ControlResponse::Data(vec![2, 0x10, 0x20]));

        assert_eq!(
            dev.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0x10, 0x20],
            })
        );
        assert_eq!(dev.pop_output_report(), None);
    }

    #[test]
    fn set_report_naks_status_stage_until_output_queue_drained() {
        let report = sample_report_descriptor_output_with_id();
        let dev_handle = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            true,
            None,
            None,
            None,
        );
        dev_handle.set_max_pending_output_reports(1);

        let mut dev = AttachedUsbDevice::new(Box::new(dev_handle.clone()));

        // Configure the device so interrupt OUT is accepted.
        assert_eq!(
            dev.handle_setup(SetupPacket {
                bm_request_type: 0x00,
                b_request: USB_REQUEST_SET_CONFIGURATION,
                w_value: 1,
                w_index: 0,
                w_length: 0,
            }),
            UsbOutResult::Ack
        );
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));

        // Fill the output report queue to capacity (1).
        assert_eq!(
            dev.handle_out(1, &[2, 0x10, 0x20]),
            UsbOutResult::Ack
        );

        // Begin a control-OUT SET_REPORT transfer while the queue is full.
        assert_eq!(
            dev.handle_setup(SetupPacket {
                bm_request_type: 0x21, // HostToDevice | Class | Interface
                b_request: HID_REQUEST_SET_REPORT,
                w_value: (2u16 << 8) | 2u16, // Output, report ID 2
                w_index: 0,
                w_length: 3,
            }),
            UsbOutResult::Ack
        );
        // DATA stage completes (ACKed), but the device must NAK the STATUS stage until there's
        // room to queue the report.
        assert_eq!(dev.handle_out(0, &[2, 0xaa, 0xbb]), UsbOutResult::Ack);
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Nak);
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Nak);

        // Drain one report (simulating the host consuming it) to allow the status stage to finish.
        assert_eq!(
            dev_handle.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0x10, 0x20],
            })
        );

        // STATUS stage should now complete, enqueueing exactly one report.
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
        assert_eq!(
            dev_handle.pop_output_report(),
            Some(UsbHidPassthroughOutputReport {
                report_type: 2,
                report_id: 2,
                data: vec![0xaa, 0xbb],
            })
        );
        assert_eq!(dev_handle.pop_output_report(), None);
    }

    #[test]
    fn input_and_output_queues_are_bounded() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
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
        loop {
            match dev.handle_in_transfer(INTERRUPT_IN_EP, 64) {
                UsbInResult::Data(r) => last = Some(r),
                UsbInResult::Nak => break,
                UsbInResult::Stall => panic!("unexpected stall draining input reports"),
                UsbInResult::Timeout => panic!("unexpected timeout draining input reports"),
            }
        }
        assert_eq!(
            last.unwrap(),
            vec![1, (DEFAULT_MAX_PENDING_INPUT_REPORTS + 49) as u8, 0, 0, 0]
        );

        // Overflow output queue.
        for i in 0..(DEFAULT_MAX_PENDING_OUTPUT_REPORTS + 17) {
            let res = dev.handle_interrupt_out(0x01, &[1, i as u8]);
            if i < DEFAULT_MAX_PENDING_OUTPUT_REPORTS {
                assert_eq!(res, UsbOutResult::Ack);
            } else {
                assert_eq!(res, UsbOutResult::Nak);
            }
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
                data: vec![(DEFAULT_MAX_PENDING_OUTPUT_REPORTS - 1) as u8]
            }
        );
    }

    #[test]
    fn complete_feature_report_request_pads_payload_to_descriptor_length() {
        let report = sample_report_descriptor_feature_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // Descriptor defines feature report ID 5 with 4 bytes of payload (5 bytes total including ID).
        let setup = SetupPacket {
            bm_request_type: 0xa1, // DeviceToHost | Class | Interface
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 5u16, // Feature, report ID 5
            w_index: 0,
            w_length: 64,
        };

        assert_eq!(dev.handle_control_request(setup, None), ControlResponse::Nak);
        let req = dev
            .pop_feature_report_request()
            .expect("expected host feature report request");
        assert_eq!(req.report_id, 5);

        // Host provides a short payload; device model should zero-pad to the descriptor length.
        assert!(dev.complete_feature_report_request(req.request_id, req.report_id, &[0x11, 0x22]));

        let resp = dev.handle_control_request(setup, None);
        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data, vec![5, 0x11, 0x22, 0, 0]);
    }

    #[test]
    fn complete_feature_report_request_truncates_payload_to_descriptor_length() {
        let report = sample_report_descriptor_feature_with_id();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // Descriptor defines feature report ID 5 with 4 bytes of payload (5 bytes total including ID).
        let setup = SetupPacket {
            bm_request_type: 0xa1, // DeviceToHost | Class | Interface
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 5u16, // Feature, report ID 5
            w_index: 0,
            w_length: 64,
        };

        assert_eq!(dev.handle_control_request(setup, None), ControlResponse::Nak);
        let req = dev
            .pop_feature_report_request()
            .expect("expected host feature report request");
        assert_eq!(req.report_id, 5);

        // Host provides a long payload; device model should truncate to the descriptor length.
        assert!(dev.complete_feature_report_request(
            req.request_id,
            req.report_id,
            &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        ));

        let resp = dev.handle_control_request(setup, None);
        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data, vec![5, 0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[test]
    fn complete_feature_report_request_caps_unknown_report_payload_length() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );
        configure_device(&mut dev);

        // Feature report ID 1 is not present in the descriptor; cached payloads should be capped.
        let setup = SetupPacket {
            bm_request_type: 0xa1, // DeviceToHost | Class | Interface
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 1u16, // Feature, report ID 1
            w_index: 0,
            w_length: 6000,
        };

        assert_eq!(dev.handle_control_request(setup, None), ControlResponse::Nak);
        let req = dev
            .pop_feature_report_request()
            .expect("expected host feature report request");
        assert_eq!(req.report_id, 1);

        let payload = vec![0x55u8; MAX_HID_SET_REPORT_BYTES + 123];
        assert!(dev.complete_feature_report_request(req.request_id, req.report_id, &payload));

        let resp = dev.handle_control_request(setup, None);
        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data.len(), MAX_HID_SET_REPORT_BYTES + 1);
        assert_eq!(data[0], 1);
        assert!(data[1..].iter().all(|&b| b == 0x55));
    }

    #[test]
    fn get_report_feature_completes_asynchronously_and_preserves_w_length() {
        let report = sample_report_descriptor_with_ids();
        let mut dev = UsbHidPassthroughHandle::new(
            0x1234,
            0x5678,
            "Vendor".into(),
            "Product".into(),
            None,
            report,
            false,
            None,
            None,
            None,
        );

        let w_length = 200;
        let setup = SetupPacket {
            bm_request_type: 0xa1, // DeviceToHost | Class | Interface
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 1u16, // Feature, report ID 1
            w_index: 0,
            w_length,
        };

        let resp = dev.handle_control_request(
            setup,
            None,
        );
        assert_eq!(resp, ControlResponse::Nak);

        let req = dev
            .pop_feature_report_request()
            .expect("expected host feature report request");
        assert_eq!(req.report_id, 1);

        // Host returns payload bytes (report ID prefix is injected by the device model).
        dev.complete_feature_report_request(req.request_id, req.report_id, &vec![0; 199]);

        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0xa1, // DeviceToHost | Class | Interface
                b_request: HID_REQUEST_GET_REPORT,
                w_value: (3u16 << 8) | 1u16, // Feature, report ID 1
                w_index: 0,
                w_length,
            },
            None,
        );
        let ControlResponse::Data(data) = resp else {
            panic!("expected data response, got {resp:?}");
        };
        assert_eq!(data.len(), w_length as usize);
    }
}
