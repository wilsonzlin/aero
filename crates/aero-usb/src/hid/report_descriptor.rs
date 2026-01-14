use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

fn default_true() -> bool {
    true
}

/// WebHID-like view of a parsed HID report descriptor.
///
/// This is intentionally a minimal subset that is sufficient for:
/// - parsing the static keyboard/mouse descriptors we ship today
/// - synthesising descriptors from WebHID-style metadata (collections/reports/items)
/// - round-tripping (parse -> synthesize -> parse) for tests
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidCollectionInfo {
    pub usage_page: u32,
    pub usage: u32,
    pub collection_type: u8,
    pub input_reports: Vec<HidReportInfo>,
    pub output_reports: Vec<HidReportInfo>,
    pub feature_reports: Vec<HidReportInfo>,
    pub children: Vec<HidCollectionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReportInfo {
    pub report_id: u32,
    pub items: Vec<HidReportItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReportItem {
    // Main-item (Input/Output/Feature) flag booleans, aligned with WebHID `HIDReportItem`.
    // See `docs/webhid-hid-report-descriptor-synthesis.md` for the HID bit mapping.
    pub is_array: bool,
    pub is_absolute: bool,
    pub is_buffered_bytes: bool,
    #[serde(default)]
    pub is_volatile: bool,
    pub is_constant: bool,
    #[serde(default)]
    pub is_wrapped: bool,
    #[serde(default = "default_true")]
    pub is_linear: bool,
    #[serde(default = "default_true")]
    pub has_preferred_state: bool,
    #[serde(default)]
    pub has_null: bool,
    pub is_range: bool,
    pub logical_minimum: i32,
    pub logical_maximum: i32,
    pub physical_minimum: i32,
    pub physical_maximum: i32,
    pub unit_exponent: i32,
    pub unit: u32,
    pub report_size: u32,
    pub report_count: u32,
    pub usage_page: u32,
    pub usages: Vec<u32>,
    #[serde(default)]
    pub strings: Vec<u32>,
    #[serde(default)]
    pub string_minimum: Option<u32>,
    #[serde(default)]
    pub string_maximum: Option<u32>,
    #[serde(default)]
    pub designators: Vec<u32>,
    #[serde(default)]
    pub designator_minimum: Option<u32>,
    #[serde(default)]
    pub designator_maximum: Option<u32>,
}

impl HidReportItem {
    pub fn bit_len(&self) -> u32 {
        self.report_size.saturating_mul(self.report_count)
    }
}

/// A WebHID report kind.
///
/// WebHID exposes `inputReports`, `outputReports`, and `featureReports` per collection. The same
/// report ID can appear in multiple collections (descriptor blocks), each contributing fields to a
/// single on-the-wire report.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HidReportKind {
    Input,
    Output,
    Feature,
}

/// Maximum allowed HID collection nesting depth.
///
/// This is a guardrail against pathological/buggy metadata (especially when sourced from WebHID)
/// that could overflow the call stack during synthesis.
const MAX_COLLECTION_DEPTH: usize = 32;

/// Maximum allowed report size (in bits).
///
/// `REPORT_SIZE` is commonly encoded as a 1-byte HID global item; values above 255 are invalid.
const MAX_REPORT_SIZE_BITS: u32 = 255;

/// Maximum allowed report count.
///
/// `REPORT_COUNT` can be encoded in 1/2/4 bytes in the descriptor, but we cap it to keep report
/// payload sizes within a sane bound.
const MAX_REPORT_COUNT: u32 = 65_535;
const MAX_HID_USAGE_U16: u32 = u16::MAX as u32;
const MAX_USB_FULL_SPEED_INTERRUPT_PACKET_BYTES: u32 = 64;
/// Maximum on-wire HID report size for `GET_REPORT` / `SET_REPORT` control transfers.
///
/// The control transfer `wLength` field is a `u16`, so the payload (including the report ID prefix
/// when `reportId != 0`) can never exceed 65535 bytes.
const MAX_USB_CONTROL_TRANSFER_BYTES: u32 = u16::MAX as u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationSummary {
    pub has_report_ids: bool,
    pub max_input_report_bytes: u32,
    pub max_output_report_bytes: u32,
    pub max_feature_report_bytes: u32,
}

#[derive(Debug, Error)]
pub enum HidDescriptorError {
    #[error("HID report descriptor ended unexpectedly")]
    UnexpectedEof,
    #[error("HID report descriptor contains a long item (0xFE), which is not supported")]
    LongItemUnsupported,
    #[error("unsupported HID item: type={item_type} tag={tag}")]
    UnsupportedItem { item_type: u8, tag: u8 },
    #[error("invalid item size {size} for {context}")]
    InvalidItemSize { context: &'static str, size: usize },
    #[error("unbalanced HID collection stack")]
    UnbalancedCollections,
    #[error("global Push/Pop stack underflow")]
    GlobalStackUnderflow,
    #[error("main item encountered outside any collection")]
    MainItemOutsideCollection,
    #[error("local usages specify multiple usage pages ({existing:#x} vs {new:#x})")]
    MultipleUsagePages { existing: u32, new: u32 },
    #[error("usage range is incomplete (must have both Usage Minimum and Usage Maximum)")]
    IncompleteUsageRange,
    #[error("string range is incomplete (must have both String Minimum and String Maximum)")]
    IncompleteStringRange,
    #[error(
        "designator range is incomplete (must have both Designator Minimum and Designator Maximum)"
    )]
    IncompleteDesignatorRange,
    #[error("report id {report_id} is out of range (must be <= 255)")]
    InvalidReportId { report_id: u32 },
    #[error("unitExponent {unit_exponent} is out of range (must be -8..=7)")]
    InvalidUnitExponent { unit_exponent: i32 },
    #[error("is_range report items must contain at least two usages (min/max)")]
    InvalidUsageRange,
    #[error("{message} (at {path})")]
    Validation { path: String, message: String },
}

impl HidDescriptorError {
    fn at(path: impl Into<String>, message: impl Into<String>) -> Self {
        HidDescriptorError::Validation {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct ValidationPath {
    segments: Vec<String>,
}

impl ValidationPath {
    fn root_collection(index: usize) -> Self {
        Self {
            segments: vec![format!("collections[{index}]")],
        }
    }

    fn push_indexed(&mut self, name: &str, index: usize) {
        self.segments.push(format!("{name}[{index}]"));
    }

    fn pop(&mut self) {
        self.segments.pop();
    }

    fn as_string(&self) -> String {
        self.segments.join(".")
    }
}

#[derive(Debug, Default)]
struct ValidationState {
    saw_nonzero_report_id: bool,
    first_zero_report_path: Option<String>,
    report_bits: BTreeMap<(HidReportKind, u32), u32>,
    report_paths: BTreeMap<(HidReportKind, u32), String>,
}

impl ValidationState {
    fn validate_report_id(&mut self, report_id: u32, path: &str) -> Result<(), HidDescriptorError> {
        if report_id > u8::MAX as u32 {
            return Err(HidDescriptorError::at(
                path,
                format!("reportId {report_id} is out of range (expected 0..=255)"),
            ));
        }

        if report_id == 0 {
            if self.saw_nonzero_report_id {
                return Err(HidDescriptorError::at(
                    path,
                    "Found reportId 0 but other reports use non-zero reportId; when any report uses a reportId, all reports must use a non-zero reportId",
                ));
            }
            if self.first_zero_report_path.is_none() {
                self.first_zero_report_path = Some(path.to_string());
            }
            return Ok(());
        }

        if let Some(first_zero_path) = self.first_zero_report_path.as_deref() {
            return Err(HidDescriptorError::at(
                first_zero_path,
                "Found reportId 0 but other reports use non-zero reportId; when any report uses a reportId, all reports must use a non-zero reportId",
            ));
        }

        self.saw_nonzero_report_id = true;
        Ok(())
    }
}

fn max_report_bytes_from_state(
    state: &ValidationState,
    kind: HidReportKind,
) -> Result<u32, HidDescriptorError> {
    let mut max = 0u32;
    let mut found = false;

    for ((k, report_id), &bits) in state.report_bits.iter() {
        if *k != kind {
            continue;
        }

        found = true;
        let path = state
            .report_paths
            .get(&(*k, *report_id))
            .map(String::as_str)
            .unwrap_or("reportDescriptor");

        let bytes = bits.checked_add(7).ok_or_else(|| {
            HidDescriptorError::at(path, "report bit length too large to round to bytes")
        })? / 8;

        max = max.max(bytes);
    }

    if found {
        Ok(max)
    } else {
        Ok(0)
    }
}

pub fn validate_collections(
    collections: &[HidCollectionInfo],
) -> Result<ValidationSummary, HidDescriptorError> {
    let mut state = ValidationState::default();

    for (idx, collection) in collections.iter().enumerate() {
        let mut path = ValidationPath::root_collection(idx);
        validate_collection(collection, &mut path, 1, &mut state)?;
    }

    Ok(ValidationSummary {
        has_report_ids: state.saw_nonzero_report_id,
        max_input_report_bytes: max_report_bytes_from_state(&state, HidReportKind::Input)?,
        max_output_report_bytes: max_report_bytes_from_state(&state, HidReportKind::Output)?,
        max_feature_report_bytes: max_report_bytes_from_state(&state, HidReportKind::Feature)?,
    })
}

fn validate_collection(
    collection: &HidCollectionInfo,
    path: &mut ValidationPath,
    depth: usize,
    state: &mut ValidationState,
) -> Result<(), HidDescriptorError> {
    if depth > MAX_COLLECTION_DEPTH {
        return Err(HidDescriptorError::at(
            path.as_string(),
            format!("HID collection nesting exceeds max depth {MAX_COLLECTION_DEPTH}"),
        ));
    }

    if collection.usage_page > MAX_HID_USAGE_U16 {
        return Err(HidDescriptorError::at(
            path.as_string(),
            format!(
                "usagePage must be in 0..={MAX_HID_USAGE_U16} (got {})",
                collection.usage_page
            ),
        ));
    }
    if collection.usage > MAX_HID_USAGE_U16 {
        return Err(HidDescriptorError::at(
            path.as_string(),
            format!(
                "usage must be in 0..={MAX_HID_USAGE_U16} (got {})",
                collection.usage
            ),
        ));
    }

    validate_report_list(
        HidReportKind::Input,
        &collection.input_reports,
        "inputReports",
        path,
        state,
    )?;
    validate_report_list(
        HidReportKind::Output,
        &collection.output_reports,
        "outputReports",
        path,
        state,
    )?;
    validate_report_list(
        HidReportKind::Feature,
        &collection.feature_reports,
        "featureReports",
        path,
        state,
    )?;

    for (child_idx, child) in collection.children.iter().enumerate() {
        path.push_indexed("children", child_idx);
        validate_collection(child, path, depth + 1, state)?;
        path.pop();
    }

    Ok(())
}

fn validate_report_list(
    kind: HidReportKind,
    reports: &[HidReportInfo],
    segment: &str,
    path: &mut ValidationPath,
    state: &mut ValidationState,
) -> Result<(), HidDescriptorError> {
    for (report_idx, report) in reports.iter().enumerate() {
        path.push_indexed(segment, report_idx);

        let report_path = path.as_string();
        state.validate_report_id(report.report_id, &report_path)?;
        state
            .report_paths
            .entry((kind, report.report_id))
            .or_insert_with(|| report_path.clone());

        for (item_idx, item) in report.items.iter().enumerate() {
            path.push_indexed("items", item_idx);
            let item_path = path.as_string();
            let bits = validate_report_item(item, &item_path)?;
            let entry = state
                .report_bits
                .entry((kind, report.report_id))
                .or_insert(0);
            let total_bits = entry.checked_add(bits).ok_or_else(|| {
                HidDescriptorError::at(&item_path, "total report bit length overflows u32")
            })?;
            *entry = total_bits;

            let data_bytes = total_bits.checked_add(7).ok_or_else(|| {
                HidDescriptorError::at(&item_path, "report bit length too large to round to bytes")
            })? / 8;
            let report_bytes = data_bytes
                .checked_add(if report.report_id != 0 { 1 } else { 0 })
                .ok_or_else(|| {
                    HidDescriptorError::at(&item_path, "report byte length overflows u32")
                })?;

            match kind {
                HidReportKind::Input => {
                    if report_bytes > MAX_USB_FULL_SPEED_INTERRUPT_PACKET_BYTES {
                        return Err(HidDescriptorError::at(
                            &item_path,
                            format!(
                                "input report length {report_bytes} bytes exceeds max USB full-speed interrupt packet size {MAX_USB_FULL_SPEED_INTERRUPT_PACKET_BYTES}",
                            ),
                        ));
                    }
                }
                HidReportKind::Output | HidReportKind::Feature => {
                    if report_bytes > MAX_USB_CONTROL_TRANSFER_BYTES {
                        let kind_str = match kind {
                            HidReportKind::Output => "output",
                            HidReportKind::Feature => "feature",
                            HidReportKind::Input => unreachable!(),
                        };
                        return Err(HidDescriptorError::at(
                            &item_path,
                            format!(
                                "{kind_str} report length {report_bytes} bytes exceeds max USB control transfer size u16::MAX ({MAX_USB_CONTROL_TRANSFER_BYTES})",
                            ),
                        ));
                    }
                }
            }
            path.pop();
        }

        path.pop();
    }
    Ok(())
}

fn validate_report_item(item: &HidReportItem, path: &str) -> Result<u32, HidDescriptorError> {
    if item.usage_page > MAX_HID_USAGE_U16 {
        return Err(HidDescriptorError::at(
            path,
            format!(
                "usagePage must be in 0..={MAX_HID_USAGE_U16} (got {})",
                item.usage_page
            ),
        ));
    }
    for (idx, &usage) in item.usages.iter().enumerate() {
        if usage > MAX_HID_USAGE_U16 {
            return Err(HidDescriptorError::at(
                path,
                format!("usages[{idx}] must be in 0..={MAX_HID_USAGE_U16} (got {usage})"),
            ));
        }
    }

    if item.report_size == 0 || item.report_size > MAX_REPORT_SIZE_BITS {
        return Err(HidDescriptorError::at(
            path,
            format!(
                "reportSize must be in 1..={MAX_REPORT_SIZE_BITS} (got {})",
                item.report_size
            ),
        ));
    }

    let bits = item
        .report_size
        .checked_mul(item.report_count)
        .ok_or_else(|| {
            HidDescriptorError::at(
                path,
                format!(
                    "reportSize*reportCount overflows u32 ({}*{})",
                    item.report_size, item.report_count
                ),
            )
        })?;

    if item.report_count > MAX_REPORT_COUNT {
        return Err(HidDescriptorError::at(
            path,
            format!(
                "reportCount must be in 0..={MAX_REPORT_COUNT} (got {})",
                item.report_count
            ),
        ));
    }

    if !(-8..=7).contains(&item.unit_exponent) {
        return Err(HidDescriptorError::at(
            path,
            format!(
                "unitExponent must be in -8..=7 (got {})",
                item.unit_exponent
            ),
        ));
    }

    if item.logical_minimum > item.logical_maximum {
        return Err(HidDescriptorError::at(
            path,
            format!(
                "logicalMinimum must be <= logicalMaximum (got {} > {})",
                item.logical_minimum, item.logical_maximum
            ),
        ));
    }

    if item.physical_minimum > item.physical_maximum {
        return Err(HidDescriptorError::at(
            path,
            format!(
                "physicalMinimum must be <= physicalMaximum (got {} > {})",
                item.physical_minimum, item.physical_maximum
            ),
        ));
    }

    if item.is_range {
        if item.usages.len() != 2 {
            return Err(HidDescriptorError::at(
                path,
                format!(
                    "isRange=true requires usages.len() == 2 (min/max), got {}",
                    item.usages.len()
                ),
            ));
        }

        if item.usages[0] > item.usages[1] {
            return Err(HidDescriptorError::at(
                path,
                format!(
                    "isRange=true requires usages[0] <= usages[1] (got {} > {})",
                    item.usages[0], item.usages[1]
                ),
            ));
        }
    }

    Ok(bits)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemType {
    Main = 0,
    Global = 1,
    Local = 2,
}

#[derive(Debug, Clone, Default)]
struct GlobalState {
    usage_page: u32,
    logical_minimum: i32,
    logical_maximum: i32,
    physical_minimum: i32,
    physical_maximum: i32,
    unit_exponent: i32,
    unit: u32,
    report_size: u32,
    report_count: u32,
    report_id: u32,
}

#[derive(Debug, Clone, Default)]
struct LocalState {
    usage_page_override: Option<u32>,
    usages: Vec<u32>,
    usage_minimum: Option<u32>,
    usage_maximum: Option<u32>,
    strings: Vec<u32>,
    string_minimum: Option<u32>,
    string_maximum: Option<u32>,
    designators: Vec<u32>,
    designator_minimum: Option<u32>,
    designator_maximum: Option<u32>,
}

impl LocalState {
    fn reset(&mut self) {
        self.usage_page_override = None;
        self.usages.clear();
        self.usage_minimum = None;
        self.usage_maximum = None;
        self.strings.clear();
        self.string_minimum = None;
        self.string_maximum = None;
        self.designators.clear();
        self.designator_minimum = None;
        self.designator_maximum = None;
    }

    fn set_usage_page_override(&mut self, page: u32) -> Result<(), HidDescriptorError> {
        if let Some(existing) = self.usage_page_override {
            if existing != page {
                return Err(HidDescriptorError::MultipleUsagePages {
                    existing,
                    new: page,
                });
            }
        } else {
            self.usage_page_override = Some(page);
        }
        Ok(())
    }
}

fn parse_unsigned(data: &[u8]) -> u32 {
    match data.len() {
        0 => 0,
        1 => data[0] as u32,
        2 => u16::from_le_bytes([data[0], data[1]]) as u32,
        4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => unreachable!("HID short items can only have 0/1/2/4 bytes of data"),
    }
}

fn parse_signed(data: &[u8]) -> i32 {
    match data.len() {
        0 => 0,
        1 => i8::from_le_bytes([data[0]]) as i32,
        2 => i16::from_le_bytes([data[0], data[1]]) as i32,
        4 => i32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => unreachable!("HID short items can only have 0/1/2/4 bytes of data"),
    }
}

fn parse_unit_exponent(data: &[u8]) -> Result<i32, HidDescriptorError> {
    // HID 1.11: Unit Exponent is a 4-bit signed value stored in the low nibble
    // of a *single* byte. High nibble is reserved.
    if data.len() != 1 {
        return Err(HidDescriptorError::InvalidItemSize {
            context: "Unit Exponent",
            size: data.len(),
        });
    }

    let nibble = data[0] & 0x0F;
    let exponent = if (nibble & 0x08) != 0 {
        (nibble as i8) - 0x10
    } else {
        nibble as i8
    };
    Ok(exponent as i32)
}

fn parse_local_usage(
    data: &[u8],
    global_usage_page: u32,
    local: &mut LocalState,
) -> Result<u32, HidDescriptorError> {
    if data.len() == 4 {
        let raw = parse_unsigned(data);
        let page = (raw >> 16) & 0xffff;
        let usage = raw & 0xffff;
        local.set_usage_page_override(page)?;
        return Ok(usage);
    }

    if let Some(override_page) = local.usage_page_override {
        if override_page != global_usage_page {
            return Err(HidDescriptorError::MultipleUsagePages {
                existing: override_page,
                new: global_usage_page,
            });
        }
    }

    Ok(parse_unsigned(data))
}

fn get_or_create_report(reports: &mut Vec<HidReportInfo>, report_id: u32) -> &mut HidReportInfo {
    if let Some(idx) = reports.iter().position(|r| r.report_id == report_id) {
        &mut reports[idx]
    } else {
        reports.push(HidReportInfo {
            report_id,
            items: Vec::new(),
        });
        reports.last_mut().expect("just pushed")
    }
}

pub fn parse_report_descriptor(bytes: &[u8]) -> Result<Vec<HidCollectionInfo>, HidDescriptorError> {
    let mut global = GlobalState::default();
    let mut global_stack: Vec<GlobalState> = Vec::new();
    let mut local = LocalState::default();

    let mut root: Vec<HidCollectionInfo> = Vec::new();
    let mut collection_stack: Vec<HidCollectionInfo> = Vec::new();

    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let prefix = bytes[cursor];
        cursor += 1;

        if prefix == 0xfe {
            return Err(HidDescriptorError::LongItemUnsupported);
        }

        let size_code = prefix & 0b11;
        let data_len = match size_code {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!("two-bit size code"),
        };
        if cursor + data_len > bytes.len() {
            return Err(HidDescriptorError::UnexpectedEof);
        }

        let item_type = (prefix >> 2) & 0b11;
        let tag = (prefix >> 4) & 0b1111;

        let data = &bytes[cursor..cursor + data_len];
        cursor += data_len;

        match item_type {
            // Main items.
            0 => {
                match tag {
                    // Input / Output / Feature
                    8 | 9 | 11 => {
                        if data.len() != 1 && data.len() != 2 {
                            return Err(HidDescriptorError::InvalidItemSize {
                                context: "Input/Output/Feature",
                                size: data.len(),
                            });
                        }
                        let flags: u16 = if data.len() == 1 {
                            data[0] as u16
                        } else {
                            u16::from_le_bytes([data[0], data[1]])
                        };

                        let is_constant = (flags & (1 << 0)) != 0;
                        let is_array = (flags & (1 << 1)) == 0;
                        let is_absolute = (flags & (1 << 2)) == 0;
                        let is_wrapped = (flags & (1 << 3)) != 0;
                        let is_linear = (flags & (1 << 4)) == 0;
                        let has_preferred_state = (flags & (1 << 5)) == 0;
                        let has_null = (flags & (1 << 6)) != 0;
                        // HID 1.11:
                        // - Input: bit7 is Bit Field / Buffered Bytes, bit8+ reserved.
                        // - Output/Feature: bit7 is Non Volatile / Volatile, bit8 is Bit Field /
                        //   Buffered Bytes.
                        let (is_buffered_bytes, is_volatile) = match tag {
                            // Input main items have no Volatile flag; bit7 is Buffered Bytes.
                            8 => ((flags & (1 << 7)) != 0, false),
                            // Output/Feature main items use bit7 for Volatile and bit8 for
                            // Buffered Bytes.
                            9 | 11 => ((flags & (1 << 8)) != 0, (flags & (1 << 7)) != 0),
                            _ => unreachable!(),
                        };

                        let usage_page = local.usage_page_override.unwrap_or(global.usage_page);
                        let (is_range, usages) = match (local.usage_minimum, local.usage_maximum) {
                            (Some(min), Some(max)) => (true, vec![min, max]),
                            (None, None) => (false, local.usages.clone()),
                            _ => return Err(HidDescriptorError::IncompleteUsageRange),
                        };

                        let (string_minimum, string_maximum) =
                            match (local.string_minimum, local.string_maximum) {
                                (Some(min), Some(max)) => (Some(min), Some(max)),
                                (None, None) => (None, None),
                                _ => return Err(HidDescriptorError::IncompleteStringRange),
                            };
                        let (designator_minimum, designator_maximum) =
                            match (local.designator_minimum, local.designator_maximum) {
                                (Some(min), Some(max)) => (Some(min), Some(max)),
                                (None, None) => (None, None),
                                _ => return Err(HidDescriptorError::IncompleteDesignatorRange),
                            };

                        let item = HidReportItem {
                            is_array,
                            is_absolute,
                            is_buffered_bytes,
                            is_volatile,
                            is_constant,
                            is_wrapped,
                            is_linear,
                            has_preferred_state,
                            has_null,
                            is_range,
                            logical_minimum: global.logical_minimum,
                            logical_maximum: global.logical_maximum,
                            physical_minimum: global.physical_minimum,
                            physical_maximum: global.physical_maximum,
                            unit_exponent: global.unit_exponent,
                            unit: global.unit,
                            report_size: global.report_size,
                            report_count: global.report_count,
                            usage_page,
                            usages,
                            strings: local.strings.clone(),
                            string_minimum,
                            string_maximum,
                            designators: local.designators.clone(),
                            designator_minimum,
                            designator_maximum,
                        };

                        let current = collection_stack
                            .last_mut()
                            .ok_or(HidDescriptorError::MainItemOutsideCollection)?;

                        let report = match tag {
                            8 => get_or_create_report(&mut current.input_reports, global.report_id),
                            9 => {
                                get_or_create_report(&mut current.output_reports, global.report_id)
                            }
                            11 => {
                                get_or_create_report(&mut current.feature_reports, global.report_id)
                            }
                            _ => unreachable!(),
                        };
                        report.items.push(item);

                        local.reset();
                    }
                    // Collection
                    10 => {
                        if data.len() != 1 {
                            return Err(HidDescriptorError::InvalidItemSize {
                                context: "Collection",
                                size: data.len(),
                            });
                        }

                        let usage_page = local.usage_page_override.unwrap_or(global.usage_page);
                        let usage = local
                            .usages
                            .first()
                            .copied()
                            .or(local.usage_minimum)
                            .unwrap_or_default();

                        collection_stack.push(HidCollectionInfo {
                            usage_page,
                            usage,
                            collection_type: data[0],
                            input_reports: Vec::new(),
                            output_reports: Vec::new(),
                            feature_reports: Vec::new(),
                            children: Vec::new(),
                        });
                        local.reset();
                    }
                    // End Collection
                    12 => {
                        if !data.is_empty() {
                            return Err(HidDescriptorError::InvalidItemSize {
                                context: "End Collection",
                                size: data.len(),
                            });
                        }

                        local.reset();
                        let finished = collection_stack
                            .pop()
                            .ok_or(HidDescriptorError::UnbalancedCollections)?;
                        if let Some(parent) = collection_stack.last_mut() {
                            parent.children.push(finished);
                        } else {
                            root.push(finished);
                        }
                    }
                    _ => {
                        return Err(HidDescriptorError::UnsupportedItem { item_type, tag });
                    }
                }
            }
            // Global items.
            1 => match tag {
                0 => global.usage_page = parse_unsigned(data),
                1 => global.logical_minimum = parse_signed(data),
                2 => global.logical_maximum = parse_signed(data),
                3 => global.physical_minimum = parse_signed(data),
                4 => global.physical_maximum = parse_signed(data),
                5 => global.unit_exponent = parse_unit_exponent(data)?,
                6 => global.unit = parse_unsigned(data),
                7 => global.report_size = parse_unsigned(data),
                8 => global.report_id = parse_unsigned(data),
                9 => global.report_count = parse_unsigned(data),
                10 => {
                    if !data.is_empty() {
                        return Err(HidDescriptorError::InvalidItemSize {
                            context: "Push",
                            size: data.len(),
                        });
                    }
                    global_stack.push(global.clone());
                }
                11 => {
                    if !data.is_empty() {
                        return Err(HidDescriptorError::InvalidItemSize {
                            context: "Pop",
                            size: data.len(),
                        });
                    }
                    global = global_stack
                        .pop()
                        .ok_or(HidDescriptorError::GlobalStackUnderflow)?;
                }
                _ => {
                    return Err(HidDescriptorError::UnsupportedItem { item_type, tag });
                }
            },
            // Local items.
            2 => {
                match tag {
                    // Usage
                    0 => {
                        let usage = parse_local_usage(data, global.usage_page, &mut local)?;
                        local.usages.push(usage);
                    }
                    // Usage Minimum
                    1 => {
                        let usage = parse_local_usage(data, global.usage_page, &mut local)?;
                        local.usage_minimum = Some(usage);
                    }
                    // Usage Maximum
                    2 => {
                        let usage = parse_local_usage(data, global.usage_page, &mut local)?;
                        local.usage_maximum = Some(usage);
                    }
                    // Designator Index
                    3 | 10 => {
                        local.designators.push(parse_unsigned(data));
                    }
                    // Designator Minimum
                    4 | 11 => {
                        local.designator_minimum = Some(parse_unsigned(data));
                    }
                    // Designator Maximum
                    5 | 12 => {
                        local.designator_maximum = Some(parse_unsigned(data));
                    }
                    // String Index
                    7 => {
                        local.strings.push(parse_unsigned(data));
                    }
                    // String Minimum
                    8 => {
                        local.string_minimum = Some(parse_unsigned(data));
                    }
                    // String Maximum
                    9 => {
                        local.string_maximum = Some(parse_unsigned(data));
                    }
                    _ => {
                        return Err(HidDescriptorError::UnsupportedItem { item_type, tag });
                    }
                }
            }
            _ => {
                return Err(HidDescriptorError::UnsupportedItem { item_type, tag });
            }
        }
    }

    if !collection_stack.is_empty() {
        return Err(HidDescriptorError::UnbalancedCollections);
    }

    Ok(root)
}

fn encode_unsigned(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn emit_unsigned(
    out: &mut Vec<u8>,
    item_type: ItemType,
    tag: u8,
    value: u32,
) -> Result<(), HidDescriptorError> {
    let bytes4 = encode_unsigned(value);
    let data: &[u8] = if value <= u8::MAX as u32 {
        &bytes4[..1]
    } else if value <= u16::MAX as u32 {
        &bytes4[..2]
    } else {
        &bytes4[..4]
    };
    emit_item(out, item_type, tag, data)
}

fn emit_signed(
    out: &mut Vec<u8>,
    item_type: ItemType,
    tag: u8,
    value: i32,
) -> Result<(), HidDescriptorError> {
    let bytes4 = value.to_le_bytes();
    let data: &[u8] = if (i8::MIN as i32..=i8::MAX as i32).contains(&value) {
        &bytes4[..1]
    } else if (i16::MIN as i32..=i16::MAX as i32).contains(&value) {
        &bytes4[..2]
    } else {
        &bytes4[..4]
    };
    emit_item(out, item_type, tag, data)
}

fn emit_unit_exponent(out: &mut Vec<u8>, unit_exponent: i32) -> Result<(), HidDescriptorError> {
    if !(-8..=7).contains(&unit_exponent) {
        return Err(HidDescriptorError::InvalidUnitExponent { unit_exponent });
    }

    // HID 1.11 Unit Exponent (0x55): 4-bit signed, stored in the low nibble.
    // High nibble must be 0.
    let encoded = (unit_exponent as i8 as u8) & 0x0F;
    emit_item(out, ItemType::Global, 5, &[encoded])
}

fn emit_item(
    out: &mut Vec<u8>,
    item_type: ItemType,
    tag: u8,
    data: &[u8],
) -> Result<(), HidDescriptorError> {
    // USB HID encodes the report descriptor length as a u16 (wDescriptorLength in the HID
    // descriptor), so we must never emit a descriptor longer than 65535 bytes. Enforce this as we
    // build the byte stream to avoid allocating absurdly large buffers for malformed metadata.
    let added = 1usize.saturating_add(data.len());
    let next_len = out.len().saturating_add(added);
    if next_len > u16::MAX as usize {
        return Err(HidDescriptorError::at(
            "reportDescriptor",
            format!(
                "HID report descriptor length {} exceeds u16::MAX ({})",
                next_len,
                u16::MAX
            ),
        ));
    }

    let size_code = match data.len() {
        0 => 0,
        1 => 1,
        2 => 2,
        4 => 3,
        other => {
            return Err(HidDescriptorError::InvalidItemSize {
                context: "short item encoding",
                size: other,
            });
        }
    };

    let prefix = ((tag & 0b1111) << 4) | ((item_type as u8) << 2) | size_code;
    out.push(prefix);
    out.extend_from_slice(data);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportKind {
    Input,
    Output,
    Feature,
}

fn synthesize_report(
    out: &mut Vec<u8>,
    kind: ReportKind,
    report: &HidReportInfo,
) -> Result<(), HidDescriptorError> {
    if report.report_id != 0 {
        if report.report_id > u8::MAX as u32 {
            return Err(HidDescriptorError::InvalidReportId {
                report_id: report.report_id,
            });
        }
        emit_unsigned(out, ItemType::Global, 8, report.report_id)?;
    }

    let main_tag = match kind {
        ReportKind::Input => 8,
        ReportKind::Output => 9,
        ReportKind::Feature => 11,
    };

    for item in &report.items {
        emit_unsigned(out, ItemType::Global, 0, item.usage_page)?;
        emit_signed(out, ItemType::Global, 1, item.logical_minimum)?;
        emit_signed(out, ItemType::Global, 2, item.logical_maximum)?;
        emit_signed(out, ItemType::Global, 3, item.physical_minimum)?;
        emit_signed(out, ItemType::Global, 4, item.physical_maximum)?;
        emit_unit_exponent(out, item.unit_exponent)?;
        emit_unsigned(out, ItemType::Global, 6, item.unit)?;
        emit_unsigned(out, ItemType::Global, 7, item.report_size)?;
        emit_unsigned(out, ItemType::Global, 9, item.report_count)?;

        if item.is_range {
            if item.usages.len() < 2 {
                return Err(HidDescriptorError::InvalidUsageRange);
            }
            emit_unsigned(out, ItemType::Local, 1, item.usages[0])?;
            emit_unsigned(out, ItemType::Local, 2, item.usages[1])?;
        } else {
            for &usage in &item.usages {
                emit_unsigned(out, ItemType::Local, 0, usage)?;
            }
        }

        match (item.string_minimum, item.string_maximum) {
            (Some(min), Some(max)) => {
                emit_unsigned(out, ItemType::Local, 8, min)?;
                emit_unsigned(out, ItemType::Local, 9, max)?;
            }
            (None, None) => {
                for &string in &item.strings {
                    emit_unsigned(out, ItemType::Local, 7, string)?;
                }
            }
            _ => return Err(HidDescriptorError::IncompleteStringRange),
        }

        match (item.designator_minimum, item.designator_maximum) {
            (Some(min), Some(max)) => {
                emit_unsigned(out, ItemType::Local, 4, min)?;
                emit_unsigned(out, ItemType::Local, 5, max)?;
            }
            (None, None) => {
                for &designator in &item.designators {
                    emit_unsigned(out, ItemType::Local, 3, designator)?;
                }
            }
            _ => return Err(HidDescriptorError::IncompleteDesignatorRange),
        }

        let mut flags: u16 = 0;
        if item.is_constant {
            flags |= 1 << 0;
        }
        if !item.is_array {
            flags |= 1 << 1;
        }
        if !item.is_absolute {
            flags |= 1 << 2;
        }
        if item.is_wrapped {
            flags |= 1 << 3;
        }
        if !item.is_linear {
            flags |= 1 << 4;
        }
        if !item.has_preferred_state {
            flags |= 1 << 5;
        }
        if item.has_null {
            flags |= 1 << 6;
        }
        match kind {
            // HID 1.11 Input main item uses bit7 for Buffered Bytes and has no Volatile flag.
            ReportKind::Input => {
                if item.is_buffered_bytes {
                    flags |= 1 << 7;
                }
            }
            // HID 1.11 Output/Feature main items use bit7 for Volatile and bit8 for Buffered
            // Bytes.
            ReportKind::Output | ReportKind::Feature => {
                if item.is_volatile {
                    flags |= 1 << 7;
                }
                if item.is_buffered_bytes {
                    flags |= 1 << 8;
                }
            }
        }

        match kind {
            ReportKind::Input => {
                // Prefer the canonical 1-byte encoding for Input items (bit7 is in-range).
                emit_item(out, ItemType::Main, main_tag, &[flags as u8])?;
            }
            ReportKind::Output | ReportKind::Feature => {
                if flags <= u8::MAX as u16 {
                    emit_item(out, ItemType::Main, main_tag, &[flags as u8])?;
                } else {
                    emit_item(out, ItemType::Main, main_tag, &flags.to_le_bytes())?;
                }
            }
        }
    }

    Ok(())
}

fn synthesize_collection(
    out: &mut Vec<u8>,
    collection: &HidCollectionInfo,
) -> Result<(), HidDescriptorError> {
    emit_unsigned(out, ItemType::Global, 0, collection.usage_page)?;
    emit_unsigned(out, ItemType::Local, 0, collection.usage)?;
    emit_item(out, ItemType::Main, 10, &[collection.collection_type])?;

    for report in &collection.input_reports {
        synthesize_report(out, ReportKind::Input, report)?;
    }
    for report in &collection.output_reports {
        synthesize_report(out, ReportKind::Output, report)?;
    }
    for report in &collection.feature_reports {
        synthesize_report(out, ReportKind::Feature, report)?;
    }
    for child in &collection.children {
        synthesize_collection(out, child)?;
    }

    emit_item(out, ItemType::Main, 12, &[])?;
    Ok(())
}

pub fn synthesize_report_descriptor(
    collections: &[HidCollectionInfo],
) -> Result<Vec<u8>, HidDescriptorError> {
    // Validate upfront so we fail deterministically with a pathful error instead of emitting a
    // descriptor that Windows may reject or misinterpret.
    let _summary = validate_collections(collections)?;

    let mut out = Vec::new();
    for collection in collections {
        synthesize_collection(&mut out, collection)?;
    }
    if out.len() > u16::MAX as usize {
        return Err(HidDescriptorError::at(
            "reportDescriptor",
            format!(
                "HID report descriptor length {} exceeds u16::MAX ({})",
                out.len(),
                u16::MAX
            ),
        ));
    }
    Ok(out)
}

enum Frame<'a> {
    Collections {
        collections: &'a [HidCollectionInfo],
        next_idx: usize,
    },
    Collection {
        collection: &'a HidCollectionInfo,
        stage: u8,
        report_idx: usize,
        item_idx: usize,
    },
}

/// Iterates over report items in the same deterministic order used by
/// [`synthesize_report_descriptor`].
///
/// Order:
/// - collection pre-order (visit parent before children)
/// - within a collection: input reports/items, output reports/items, feature reports/items, then
///   children.
pub fn iter_reports_in_synth_order<'a>(
    collections: &'a [HidCollectionInfo],
) -> impl Iterator<Item = (HidReportKind, u32, &'a HidReportItem)> + 'a {
    ReportsInSynthOrder::new(collections)
}

struct ReportsInSynthOrder<'a> {
    stack: Vec<Frame<'a>>,
}

impl<'a> ReportsInSynthOrder<'a> {
    fn new(collections: &'a [HidCollectionInfo]) -> Self {
        Self {
            stack: vec![Frame::Collections {
                collections,
                next_idx: 0,
            }],
        }
    }
}

impl<'a> Iterator for ReportsInSynthOrder<'a> {
    type Item = (HidReportKind, u32, &'a HidReportItem);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let frame = self.stack.pop()?;
            match frame {
                Frame::Collections {
                    collections,
                    mut next_idx,
                } => {
                    if next_idx >= collections.len() {
                        continue;
                    }

                    let collection = &collections[next_idx];
                    next_idx += 1;

                    // Ensure siblings are visited after this collection (pre-order).
                    self.stack.push(Frame::Collections {
                        collections,
                        next_idx,
                    });
                    self.stack.push(Frame::Collection {
                        collection,
                        stage: 0,
                        report_idx: 0,
                        item_idx: 0,
                    });
                }
                Frame::Collection {
                    collection,
                    mut stage,
                    mut report_idx,
                    mut item_idx,
                } => loop {
                    let (kind, reports): (Option<HidReportKind>, &[HidReportInfo]) = match stage {
                        0 => (
                            Some(HidReportKind::Input),
                            collection.input_reports.as_slice(),
                        ),
                        1 => (
                            Some(HidReportKind::Output),
                            collection.output_reports.as_slice(),
                        ),
                        2 => (
                            Some(HidReportKind::Feature),
                            collection.feature_reports.as_slice(),
                        ),
                        _ => (None, &[]),
                    };

                    if let Some(kind) = kind {
                        if let Some(report) = reports.get(report_idx) {
                            if let Some(item) = report.items.get(item_idx) {
                                item_idx += 1;
                                self.stack.push(Frame::Collection {
                                    collection,
                                    stage,
                                    report_idx,
                                    item_idx,
                                });
                                return Some((kind, report.report_id, item));
                            }

                            report_idx += 1;
                            item_idx = 0;
                            continue;
                        }
                    }

                    stage += 1;
                    report_idx = 0;
                    item_idx = 0;

                    if stage >= 3 {
                        self.stack.push(Frame::Collections {
                            collections: &collection.children,
                            next_idx: 0,
                        });
                        break;
                    }
                },
            }
        }
    }
}

/// Aggregates report items by `(kind, report_id)` across the entire collection tree.
///
/// WebHID report IDs are scoped to the full descriptor, not to individual collections. A report ID
/// may appear in multiple collections, each contributing fields to a single on-the-wire report.
pub fn aggregate_reports(
    collections: &[HidCollectionInfo],
) -> BTreeMap<(HidReportKind, u32), Vec<HidReportItem>> {
    let mut out: BTreeMap<(HidReportKind, u32), Vec<HidReportItem>> = BTreeMap::new();
    for (kind, report_id, item) in iter_reports_in_synth_order(collections) {
        out.entry((kind, report_id)).or_default().push(item.clone());
    }
    out
}

fn report_bits(items: &[HidReportItem]) -> u32 {
    // The public size helpers (`report_bytes_for_id`, `max_*_report_bytes`) are best-effort and
    // should not panic on pathological descriptors. Use a wide, saturating accumulator and clamp
    // to `u32::MAX` if the total exceeds the representable range.
    let bits = items
        .iter()
        .map(|item| u64::from(item.bit_len()))
        .fold(0u64, |acc, v| acc.saturating_add(v));
    u32::try_from(bits).unwrap_or(u32::MAX)
}

/// Returns the total number of bits for a given `(kind, report_id)` across the whole descriptor.
pub fn report_bits_for_id(
    collections: &[HidCollectionInfo],
    kind: HidReportKind,
    report_id: u32,
) -> u32 {
    let aggregated = aggregate_reports(collections);
    aggregated
        .get(&(kind, report_id))
        .map(|items| report_bits(items))
        .unwrap_or(0)
}

/// Returns the report payload length in bytes (excluding the report ID prefix byte).
pub fn report_bytes_for_id(
    collections: &[HidCollectionInfo],
    kind: HidReportKind,
    report_id: u32,
) -> usize {
    let bits = report_bits_for_id(collections, kind, report_id);
    let bytes = u64::from(bits).div_ceil(8);
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

fn max_report_bytes(collections: &[HidCollectionInfo], kind: HidReportKind) -> usize {
    let aggregated = aggregate_reports(collections);
    aggregated
        .iter()
        .filter(|((k, _), _)| *k == kind)
        .map(|(_, items)| {
            let bits = report_bits(items);
            let bytes = u64::from(bits).div_ceil(8);
            usize::try_from(bytes).unwrap_or(usize::MAX)
        })
        .max()
        .unwrap_or(0)
}

pub fn max_input_report_bytes(collections: &[HidCollectionInfo]) -> usize {
    max_report_bytes(collections, HidReportKind::Input)
}

pub fn max_output_report_bytes(collections: &[HidCollectionInfo]) -> usize {
    max_report_bytes(collections, HidReportKind::Output)
}

pub fn max_feature_report_bytes(collections: &[HidCollectionInfo]) -> usize {
    max_report_bytes(collections, HidReportKind::Feature)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(desc: &[u8]) {
        let parsed = parse_report_descriptor(desc).unwrap();
        let synthesized = synthesize_report_descriptor(&parsed).unwrap();
        let reparsed = parse_report_descriptor(&synthesized).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn roundtrip_preserves_string_and_designator_locals() {
        // Descriptor is encoded in the same deterministic order used by the synthesizer so we can
        // assert byte-for-byte identity after parse -> synth.
        let desc = [
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x00, // Usage (Undefined)
            0xA1, 0x01, // Collection (Application)
            // Item 1: explicit String/Designator indices.
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x15, 0x00, // Logical Minimum (0)
            0x26, 0xFF, 0x00, // Logical Maximum (255)
            0x35, 0x00, // Physical Minimum (0)
            0x45, 0x00, // Physical Maximum (0)
            0x55, 0x00, // Unit Exponent (0)
            0x65, 0x00, // Unit (None)
            0x75, 0x08, // Report Size (8)
            0x95, 0x01, // Report Count (1)
            0x09, 0x00, // Usage (Undefined)
            0x79, 0x01, // String Index (1)
            0x39, 0x02, // Designator Index (2)
            0x81, 0x02, // Input (Data,Var,Abs)
            // Item 2: String/Designator ranges.
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x15, 0x00, // Logical Minimum (0)
            0x26, 0xFF, 0x00, // Logical Maximum (255)
            0x35, 0x00, // Physical Minimum (0)
            0x45, 0x00, // Physical Maximum (0)
            0x55, 0x00, // Unit Exponent (0)
            0x65, 0x00, // Unit (None)
            0x75, 0x08, // Report Size (8)
            0x95, 0x01, // Report Count (1)
            0x19, 0x01, // Usage Minimum (1)
            0x29, 0x03, // Usage Maximum (3)
            0x89, 0x10, // String Minimum (0x10)
            0x99, 0x12, // String Maximum (0x12)
            0x49, 0x20, // Designator Minimum (0x20)
            0x59, 0x22, // Designator Maximum (0x22)
            0x81, 0x02, // Input (Data,Var,Abs)
            0xC0, // End Collection
        ];

        let parsed = parse_report_descriptor(&desc).unwrap();
        let synthesized = synthesize_report_descriptor(&parsed).unwrap();
        assert_eq!(synthesized, desc);
        let reparsed = parse_report_descriptor(&synthesized).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn synth_includes_report_id() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x06,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0x07,
                    usages: vec![0xe0],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(2).any(|w| w == [0x85, 0x01]),
            "expected Report ID item (0x85 0x01) in synthesized descriptor: {desc:02x?}"
        );
        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn synth_uses_two_byte_encodings_for_16bit_usage_page_and_usage() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0xff00,
            usage: 0x1234,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0xff00,
                    usages: vec![0x1234],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(3).any(|w| w == [0x06, 0x00, 0xff]),
            "expected Usage Page (0x06 0x00 0xff) in descriptor: {desc:02x?}"
        );
        assert!(
            desc.windows(3).any(|w| w == [0x0a, 0x34, 0x12]),
            "expected 2-byte Usage (0x0a 0x34 0x12) in descriptor: {desc:02x?}"
        );
        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn synth_encodes_negative_logical_minimum_as_one_byte_twos_complement() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: -127,
                    logical_maximum: 127,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(2).any(|w| w == [0x15, 0x81]),
            "expected Logical Minimum (-127) encoding (0x15 0x81): {desc:02x?}"
        );
        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn synth_encodes_negative_unit_exponent_as_low_nibble() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: -1,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(2).any(|w| w == [0x55, 0x0F]),
            "expected Unit Exponent (-1) encoding (0x55 0x0f): {desc:02x?}"
        );
        assert!(
            !desc.windows(2).any(|w| w == [0x55, 0xFF]),
            "Unit Exponent must not be encoded as signed i8 (0x55 0xff): {desc:02x?}"
        );
    }

    #[test]
    fn parse_decodes_unit_exponent_as_4bit_signed() {
        // Minimal descriptor with a Unit Exponent global item used by a single Input item.
        let desc = [
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x02, // Usage (Mouse)
            0xA1, 0x01, // Collection (Application)
            0x55, 0x0E, // Unit Exponent (-2) encoded in low nibble
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x01, // Logical Maximum (1)
            0x75, 0x01, // Report Size (1)
            0x95, 0x01, // Report Count (1)
            0x81, 0x02, // Input (Data,Var,Abs)
            0xC0, // End Collection
        ];

        let parsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].input_reports.len(), 1);
        assert_eq!(parsed[0].input_reports[0].items.len(), 1);
        assert_eq!(parsed[0].input_reports[0].items[0].unit_exponent, -2);
    }

    #[test]
    fn synth_rejects_unit_exponent_out_of_range() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 8,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].inputReports[0].items[0]");
                assert!(message.contains("unitExponent"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_collection_usage_page_out_of_range() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x1_0000,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0]");
                assert!(message.contains("usagePage"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_usage_out_of_range() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x1_0000],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].inputReports[0].items[0]");
                assert!(message.contains("usages[0]"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_feature_report_larger_than_usb_control_transfer_without_report_id() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![],
            feature_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 16,
                    report_count: 32_768,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].featureReports[0].items[0]");
                assert!(message.contains("control"));
                assert!(message.contains("65536"));
                assert!(message.contains("65535"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_input_report_larger_than_full_speed_interrupt_packet() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 65,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].inputReports[0].items[0]");
                assert!(message.contains("interrupt"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_report_id_prefix_overflowing_interrupt_packet() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 64,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].inputReports[0].items[0]");
                assert!(message.contains("interrupt"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_allows_output_report_larger_than_full_speed_interrupt_packet() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 65,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn synth_allows_output_report_id_prefix_overflowing_interrupt_packet() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 64,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn synth_rejects_output_report_larger_than_usb_control_transfer() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: u16::MAX as u32,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].outputReports[0].items[0]");
                assert!(message.contains("control"));
                assert!(message.contains("65536"));
                assert!(message.contains("65535"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_feature_report_larger_than_usb_control_transfer() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![],
            feature_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: u16::MAX as u32,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].featureReports[0].items[0]");
                assert!(message.contains("control"));
                assert!(message.contains("65536"));
                assert!(message.contains("65535"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_output_report_larger_than_control_transfer_limit() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    // 65535 payload bytes + 1 report ID prefix byte = 65536 bytes on-wire.
                    report_size: 8,
                    report_count: MAX_REPORT_COUNT,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].outputReports[0].items[0]");
                assert!(message.contains("65536"));
                assert!(message.contains("65535"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_feature_report_larger_than_control_transfer_limit() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![],
            feature_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    // 65535 payload bytes + 1 report ID prefix byte = 65536 bytes on-wire.
                    report_size: 8,
                    report_count: MAX_REPORT_COUNT,
                    usage_page: 0x01,
                    usages: vec![],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].featureReports[0].items[0]");
                assert!(message.contains("65536"));
                assert!(message.contains("65535"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_range_item_with_too_many_usages() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: true,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![1, 2, 3],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "collections[0].inputReports[0].items[0]");
                assert!(message.contains("usages.len()"));
                assert!(message.contains("== 2"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn synth_rejects_report_descriptor_length_overflow() {
        // Each explicit `Usage` local item is 2 bytes when the usage fits in u8 (prefix + 1-byte
        // payload). Emit enough usages to exceed the u16 report descriptor length field.
        let usages = vec![0u32; 32_768];
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0x01,
                    usages,
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::Validation { path, message }) => {
                assert_eq!(path, "reportDescriptor");
                assert!(message.contains("length"));
                assert!(message.contains("u16::MAX"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn buffered_bytes_uses_bit7_for_input_main_items() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: true,
                    is_absolute: true,
                    is_buffered_bytes: true,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 0,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();

        assert!(
            desc.windows(2).any(|w| w == [0x81, 0x80]),
            "expected spec-canonical Input Buffered Bytes encoding (0x81 0x80): {desc:02x?}"
        );
        assert!(
            !desc.windows(3).any(|w| w == [0x82, 0x00, 0x01]),
            "did not expect Input Buffered Bytes to be encoded as a 2-byte payload (0x82 0x00 0x01): {desc:02x?}"
        );

        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn parse_input_buffered_bytes_ignores_bit8_in_two_byte_payloads() {
        // HID 1.11: For Input main items, Buffered Bytes is bit7; bit8 and above are reserved.
        let desc_bit7 = [
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x02, // Usage (Mouse)
            0xA1, 0x01, // Collection (Application)
            0x75, 0x08, // Report Size (8)
            0x95, 0x01, // Report Count (1)
            0x82, 0x80, 0x00, // Input (2-byte payload, bit7 set)
            0xC0, // End Collection
        ];

        let parsed = parse_report_descriptor(&desc_bit7).unwrap();
        assert!(parsed[0].input_reports[0].items[0].is_buffered_bytes);

        let desc_bit8 = [
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x02, // Usage (Mouse)
            0xA1, 0x01, // Collection (Application)
            0x75, 0x08, // Report Size (8)
            0x95, 0x01, // Report Count (1)
            0x82, 0x00, 0x01, // Input (2-byte payload, reserved bit8 set)
            0xC0, // End Collection
        ];

        let parsed = parse_report_descriptor(&desc_bit8).unwrap();
        assert!(!parsed[0].input_reports[0].items[0].is_buffered_bytes);
    }

    #[test]
    fn buffered_bytes_uses_bit8_for_output_main_items() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: true,
                    is_absolute: true,
                    is_buffered_bytes: true,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 0,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();

        assert!(
            desc.windows(3).any(|w| w == [0x92, 0x00, 0x01]),
            "expected spec-canonical Output Buffered Bytes encoding (0x92 0x00 0x01): {desc:02x?}"
        );

        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn volatile_sets_bit7_for_output_main_items() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: true,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: true,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 0,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();

        assert!(
            desc.windows(2).any(|w| w == [0x91, 0x80]),
            "expected spec-canonical Output Volatile encoding (0x91 0x80): {desc:02x?}"
        );

        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn hat_switch_null_state_synthesizes_to_input_0x42() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    // Spec-canonical hat switch main item flags: Data,Var,Abs,Null (0x42).
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: true,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 0,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();

        assert!(
            desc.windows(2).any(|w| w == [0x81, 0x42]),
            "expected Input item with Null State flag (0x81 0x42): {desc:02x?}"
        );

        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn main_item_flag_bitset_roundtrips_for_input_items() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: false,
                    is_buffered_bytes: true,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: true,
                    is_linear: false,
                    has_preferred_state: false,
                    has_null: true,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 1,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();

        assert!(
            desc.windows(2).any(|w| w == [0x81, 0xFE]),
            "expected Input main item flags 0xfe encoding (0x81 0xfe): {desc:02x?}"
        );

        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    #[test]
    fn main_item_flag_bitset_roundtrips_for_output_items() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: true,
                    is_volatile: true,
                    is_constant: false,
                    is_wrapped: true,
                    is_linear: false,
                    has_preferred_state: false,
                    has_null: true,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 1,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();

        assert!(
            desc.windows(3).any(|w| w == [0x92, 0xFA, 0x01]),
            "expected Output main item flags 0x01fa encoding (0x92 0xfa 0x01): {desc:02x?}"
        );

        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed);
    }

    fn simple_item(report_size: u32, report_count: u32) -> HidReportItem {
        HidReportItem {
            is_array: false,
            is_absolute: true,
            is_buffered_bytes: false,
            is_volatile: false,
            is_constant: false,
            is_wrapped: false,
            is_linear: true,
            has_preferred_state: true,
            has_null: false,
            is_range: false,
            logical_minimum: 0,
            logical_maximum: 0,
            physical_minimum: 0,
            physical_maximum: 0,
            unit_exponent: 0,
            unit: 0,
            report_size,
            report_count,
            usage_page: 0,
            usages: Vec::new(),
            strings: Vec::new(),
            string_minimum: None,
            string_maximum: None,
            designators: Vec::new(),
            designator_minimum: None,
            designator_maximum: None,
        }
    }

    #[test]
    fn iter_reports_in_synth_order_matches_collection_preorder() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0,
            usage: 0,
            collection_type: 0,
            input_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![simple_item(8, 1)],
            }],
            output_reports: vec![HidReportInfo {
                report_id: 2,
                items: vec![simple_item(16, 1)],
            }],
            feature_reports: vec![HidReportInfo {
                report_id: 3,
                items: vec![simple_item(32, 1)],
            }],
            children: vec![HidCollectionInfo {
                usage_page: 0,
                usage: 0,
                collection_type: 0,
                input_reports: vec![HidReportInfo {
                    report_id: 1,
                    items: vec![simple_item(8, 2)],
                }],
                output_reports: Vec::new(),
                feature_reports: Vec::new(),
                children: Vec::new(),
            }],
        }];

        let got: Vec<(HidReportKind, u32, u32)> = iter_reports_in_synth_order(&collections)
            .map(|(kind, report_id, item)| (kind, report_id, item.bit_len()))
            .collect();

        assert_eq!(
            got,
            vec![
                (HidReportKind::Input, 1, 8),
                (HidReportKind::Output, 2, 16),
                (HidReportKind::Feature, 3, 32),
                (HidReportKind::Input, 1, 16),
            ]
        );
    }

    #[test]
    fn aggregates_items_across_collections_with_same_report_id() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0,
            usage: 0,
            collection_type: 0,
            input_reports: vec![HidReportInfo {
                report_id: 1,
                items: vec![simple_item(8, 1)],
            }],
            output_reports: Vec::new(),
            feature_reports: Vec::new(),
            children: vec![HidCollectionInfo {
                usage_page: 0,
                usage: 0,
                collection_type: 0,
                input_reports: vec![HidReportInfo {
                    report_id: 1,
                    items: vec![simple_item(16, 1)],
                }],
                output_reports: Vec::new(),
                feature_reports: Vec::new(),
                children: Vec::new(),
            }],
        }];

        let aggregated = aggregate_reports(&collections);
        let items = aggregated
            .get(&(HidReportKind::Input, 1))
            .expect("missing aggregated report");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].bit_len(), 8);
        assert_eq!(items[1].bit_len(), 16);

        assert_eq!(
            report_bits_for_id(&collections, HidReportKind::Input, 1),
            24
        );
        assert_eq!(
            report_bytes_for_id(&collections, HidReportKind::Input, 1),
            3
        );
        assert_eq!(max_input_report_bytes(&collections), 3);
    }

    #[test]
    fn roundtrip_synthesized_descriptor() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_volatile: false,
                    is_constant: false,
                    is_wrapped: false,
                    is_linear: true,
                    has_preferred_state: true,
                    has_null: false,
                    is_range: false,
                    logical_minimum: 0,
                    logical_maximum: 127,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 8,
                    report_count: 1,
                    usage_page: 0x01,
                    usages: vec![0x30],
                    strings: vec![],
                    string_minimum: None,
                    string_maximum: None,
                    designators: vec![],
                    designator_minimum: None,
                    designator_maximum: None,
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        roundtrip(&desc);
    }
}
