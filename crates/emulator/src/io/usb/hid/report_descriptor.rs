use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_EXPANDED_USAGE_RANGE: u32 = 4096;

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
pub struct HidReportDescriptorParseResult {
    pub collections: Vec<HidCollectionInfo>,
    pub truncated_ranges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReportItem {
    pub is_array: bool,
    pub is_absolute: bool,
    pub is_buffered_bytes: bool,
    pub is_constant: bool,
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
    #[error("usage range is invalid: minimum {min:#x} > maximum {max:#x}")]
    UsageRangeMinGreaterThanMax { min: u32, max: u32 },
    #[error("report id {report_id} is out of range (must be <= 255)")]
    InvalidReportId { report_id: u32 },
    #[error("unitExponent {unit_exponent} is out of range (must be -8..=7)")]
    InvalidUnitExponent { unit_exponent: i32 },
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
}

impl LocalState {
    fn reset(&mut self) {
        self.usage_page_override = None;
        self.usages.clear();
        self.usage_minimum = None;
        self.usage_maximum = None;
    }

    fn set_usage_page_override(
        &mut self,
        page: u32,
    ) -> Result<(), HidDescriptorError> {
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

fn get_or_create_report<'a>(
    reports: &'a mut Vec<HidReportInfo>,
    report_id: u32,
) -> &'a mut HidReportInfo {
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

pub fn parse_report_descriptor(
    bytes: &[u8],
) -> Result<HidReportDescriptorParseResult, HidDescriptorError> {
    let mut global = GlobalState::default();
    let mut global_stack: Vec<GlobalState> = Vec::new();
    let mut local = LocalState::default();

    let mut root: Vec<HidCollectionInfo> = Vec::new();
    let mut collection_stack: Vec<HidCollectionInfo> = Vec::new();
    let mut truncated_ranges = false;

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
                        let is_buffered_bytes = (flags & (1 << 8)) != 0;

                        let usage_page = local.usage_page_override.unwrap_or(global.usage_page);
                        let (is_range, usages) = match (local.usage_minimum, local.usage_maximum) {
                            (Some(min), Some(max)) => {
                                if min > max {
                                    return Err(HidDescriptorError::UsageRangeMinGreaterThanMax {
                                        min,
                                        max,
                                    });
                                }
                                match max.checked_sub(min).and_then(|d| d.checked_add(1)) {
                                    Some(len) if len <= MAX_EXPANDED_USAGE_RANGE => {
                                        let mut out = Vec::with_capacity(len as usize);
                                        for u in min..=max {
                                            out.push(u);
                                        }
                                        (true, out)
                                    }
                                    _ => {
                                        truncated_ranges = true;
                                        (true, vec![min, max])
                                    }
                                }
                            }
                            (None, None) => (false, local.usages.clone()),
                            _ => return Err(HidDescriptorError::IncompleteUsageRange),
                        };

                        let item = HidReportItem {
                            is_array,
                            is_absolute,
                            is_buffered_bytes,
                            is_constant,
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
                        };

                        let current = collection_stack
                            .last_mut()
                            .ok_or(HidDescriptorError::MainItemOutsideCollection)?;

                        let report = match tag {
                            8 => get_or_create_report(&mut current.input_reports, global.report_id),
                            9 => {
                                get_or_create_report(&mut current.output_reports, global.report_id)
                            }
                            11 => get_or_create_report(
                                &mut current.feature_reports,
                                global.report_id,
                            ),
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
                        let usage = if let Some(&usage) = local.usages.first() {
                            usage
                        } else if let Some(min) = local.usage_minimum {
                            min
                        } else {
                            0
                        };

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
            1 => {
                match tag {
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
                }
            }
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

    Ok(HidReportDescriptorParseResult {
        collections: root,
        truncated_ranges,
    })
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
            if !item.usages.is_empty() {
                let mut sorted = item.usages.clone();
                sorted.sort_unstable();
                sorted.dedup();
                let min = *sorted.first().unwrap();
                let max = *sorted.last().unwrap();

                let contiguous = if min == max {
                    true
                } else if item.usages.len() == 2 {
                    // Support legacy `[min, max]` representation without needing to allocate the
                    // full expanded list.
                    true
                } else if let Some(span) = max.checked_sub(min).and_then(|d| d.checked_add(1)) {
                    span as usize == sorted.len()
                        && sorted
                            .iter()
                            .enumerate()
                            .all(|(idx, &v)| v == min + (idx as u32))
                } else {
                    false
                };

                if contiguous {
                    emit_unsigned(out, ItemType::Local, 1, min)?;
                    emit_unsigned(out, ItemType::Local, 2, max)?;
                } else {
                    for usage in sorted {
                        emit_unsigned(out, ItemType::Local, 0, usage)?;
                    }
                }
            }
        } else {
            for &usage in &item.usages {
                emit_unsigned(out, ItemType::Local, 0, usage)?;
            }
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
        if item.is_buffered_bytes {
            flags |= 1 << 8;
        }

        if item.is_buffered_bytes {
            emit_item(out, ItemType::Main, main_tag, &flags.to_le_bytes())?;
        } else {
            emit_item(out, ItemType::Main, main_tag, &[flags as u8])?;
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
    let mut out = Vec::new();
    for collection in collections {
        synthesize_collection(&mut out, collection)?;
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
                } => {
                    loop {
                        let (kind, reports): (Option<HidReportKind>, &[HidReportInfo]) =
                            match stage {
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
                    }
                }
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
    items.iter().map(HidReportItem::bit_len).sum()
}

/// Returns the total number of bits for a given `(kind, report_id)` across the whole descriptor.
pub fn report_bits_for_id(collections: &[HidCollectionInfo], kind: HidReportKind, report_id: u32) -> u32 {
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
    usize::try_from((bits + 7) / 8).unwrap_or(usize::MAX)
}

fn max_report_bytes(collections: &[HidCollectionInfo], kind: HidReportKind) -> usize {
    let aggregated = aggregate_reports(collections);
    aggregated
        .iter()
        .filter(|((k, _), _)| *k == kind)
        .map(|(_, items)| usize::try_from((report_bits(items) + 7) / 8).unwrap_or(usize::MAX))
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

    use crate::io::usb::hid::{keyboard, mouse};

    fn roundtrip(desc: &[u8]) {
        let parsed = parse_report_descriptor(desc).unwrap();
        let synthesized = synthesize_report_descriptor(&parsed.collections).unwrap();
        let reparsed = parse_report_descriptor(&synthesized).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn roundtrip_keyboard_and_mouse() {
        roundtrip(&keyboard::HID_REPORT_DESCRIPTOR);
        roundtrip(&mouse::HID_REPORT_DESCRIPTOR);
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
                    is_constant: false,
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
        assert_eq!(collections, reparsed.collections);
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
                    is_constant: false,
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
        assert_eq!(collections, reparsed.collections);
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
                    is_constant: false,
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
        assert_eq!(collections, reparsed.collections);
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
                    is_constant: false,
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
        assert_eq!(parsed.collections.len(), 1);
        assert_eq!(parsed.collections[0].input_reports.len(), 1);
        assert_eq!(parsed.collections[0].input_reports[0].items.len(), 1);
        assert_eq!(parsed.collections[0].input_reports[0].items[0].unit_exponent, -2);
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
                    is_constant: false,
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
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorError::InvalidUnitExponent { unit_exponent }) => {
                assert_eq!(unit_exponent, 8);
            }
            other => panic!("expected InvalidUnitExponent error, got {other:?}"),
        }
    }

    #[test]
    fn buffered_bytes_forces_two_byte_main_item_payload() {
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
                    is_constant: false,
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
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();

        // Input main item with 2-byte payload is encoded as:
        //   0x82, <low>, <high>
        // and "Buffered Bytes" is bit8, i.e. bit0 of <high>.
        let mut found = false;
        for win in desc.windows(3) {
            if win[0] == 0x82 && (win[2] & 0x01) != 0 {
                found = true;
                break;
            }
        }
        assert!(found, "expected 2-byte Input item with Buffered Bytes flag: {desc:02x?}");

        let reparsed = parse_report_descriptor(&desc).unwrap();
        assert_eq!(collections, reparsed.collections);
    }

    #[test]
    fn parse_expands_keyboard_modifier_usage_range() {
        let parsed = parse_report_descriptor(&keyboard::HID_REPORT_DESCRIPTOR).unwrap();
        assert!(!parsed.truncated_ranges);
        assert_eq!(parsed.collections.len(), 1);

        let collection = &parsed.collections[0];
        assert_eq!(collection.input_reports.len(), 1);
        let report = &collection.input_reports[0];
        assert_eq!(report.report_id, 0);
        let modifier_item = &report.items[0];
        assert!(modifier_item.is_range);
        assert_eq!(
            modifier_item.usages,
            vec![0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7]
        );

        let synthesized = synthesize_report_descriptor(&parsed.collections).unwrap();
        assert!(
            synthesized
                .windows(4)
                .any(|w| w == [0x19, 0xE0, 0x29, 0xE7]),
            "expected Usage Minimum/Maximum to cover E0..E7, got: {synthesized:02x?}"
        );
    }

    #[test]
    fn synth_range_single_usage_emits_min_eq_max() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x06,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_constant: false,
                    is_range: true,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 1,
                    report_count: 1,
                    usage_page: 0x07,
                    usages: vec![5],
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(4).any(|w| w == [0x19, 0x05, 0x29, 0x05]),
            "expected single-usage range to synthesize as Usage Min/Max(5): {desc:02x?}"
        );
    }

    #[test]
    fn synth_noncontiguous_range_falls_back_to_explicit_usages() {
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x06,
            collection_type: 0x01,
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    is_array: false,
                    is_absolute: true,
                    is_buffered_bytes: false,
                    is_constant: false,
                    is_range: true,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    physical_minimum: 0,
                    physical_maximum: 0,
                    unit_exponent: 0,
                    unit: 0,
                    report_size: 1,
                    report_count: 3,
                    usage_page: 0x07,
                    usages: vec![1, 3, 4],
                }],
            }],
            output_reports: vec![],
            feature_reports: vec![],
            children: vec![],
        }];

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(6).any(|w| w == [0x09, 0x01, 0x09, 0x03, 0x09, 0x04]),
            "expected explicit Usage tags for non-contiguous range: {desc:02x?}"
        );
        assert!(
            !desc.windows(4).any(|w| w == [0x19, 0x01, 0x29, 0x04]),
            "did not expect Usage Minimum/Maximum for non-contiguous usages: {desc:02x?}"
        );
    }

    fn simple_item(report_size: u32, report_count: u32) -> HidReportItem {
        HidReportItem {
            is_array: false,
            is_absolute: true,
            is_buffered_bytes: false,
            is_constant: false,
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

        assert_eq!(report_bits_for_id(&collections, HidReportKind::Input, 1), 24);
        assert_eq!(report_bytes_for_id(&collections, HidReportKind::Input, 1), 3);
        assert_eq!(max_input_report_bytes(&collections), 3);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn usage_u16ish() -> impl Strategy<Value = u32> {
        0u32..=0x03ff
    }

    fn collection_type_strategy() -> impl Strategy<Value = u8> {
        // HID 1.11 collection types are small; keep this "valid-ish" and bounded.
        0u8..=6
    }

    fn ordered_i16_pair() -> impl Strategy<Value = (i32, i32)> {
        (any::<i16>(), any::<i16>()).prop_map(|(a, b)| {
            let (min, max) = if a <= b { (a, b) } else { (b, a) };
            (min as i32, max as i32)
        })
    }

    fn item_strategy() -> impl Strategy<Value = HidReportItem> {
        (
            any::<bool>(), // is_array
            any::<bool>(), // is_absolute
            any::<bool>(), // is_constant
            any::<bool>(), // is_buffered_bytes
            any::<bool>(), // is_range
            1u32..=32u32,  // report_size
            0u32..=32u32,  // report_count
            ordered_i16_pair(),
            ordered_i16_pair(),
            -8i32..=7i32,   // unit_exponent (4-bit signed)
            0u32..=0u32,    // unit (keep simple for now)
            usage_u16ish(), // usage_page
        )
            .prop_flat_map(
                |(
                    is_array,
                    is_absolute,
                    is_constant,
                    is_buffered_bytes,
                    is_range,
                    report_size,
                    report_count,
                    (logical_minimum, logical_maximum),
                    (physical_minimum, physical_maximum),
                    unit_exponent,
                    unit,
                    usage_page,
                )| {
                    let usages = if is_range {
                        (usage_u16ish(), usage_u16ish())
                            .prop_map(|(a, b)| {
                                let (min, max) = if a <= b { (a, b) } else { (b, a) };
                                vec![min, max]
                            })
                            .boxed()
                    } else {
                        prop::collection::vec(usage_u16ish(), 0..=4).boxed()
                    };

                    usages.prop_map(move |usages| HidReportItem {
                        is_array,
                        is_absolute,
                        is_buffered_bytes,
                        is_constant,
                        is_range,
                        logical_minimum,
                        logical_maximum,
                        physical_minimum,
                        physical_maximum,
                        unit_exponent,
                        unit,
                        report_size,
                        report_count,
                        usage_page,
                        usages,
                    })
                },
            )
    }

    fn report_list_strategy(use_report_ids: bool) -> BoxedStrategy<Vec<HidReportInfo>> {
        if use_report_ids {
            // Generate a small set of unique IDs so parse doesn't need to guess whether multiple
            // occurrences should be merged.
            prop::collection::btree_set(1u32..=16, 0..=3)
                .prop_flat_map(|ids| {
                    let ids: Vec<u32> = ids.into_iter().collect();
                    prop::collection::vec(prop::collection::vec(item_strategy(), 0..=8), ids.len())
                        .prop_map(move |items| {
                            ids.iter()
                                .copied()
                                .zip(items)
                                .map(|(report_id, items)| HidReportInfo { report_id, items })
                                .collect::<Vec<_>>()
                        })
                })
                .boxed()
        } else {
            // With no Report ID items present, all fields participate in the single implicit
            // report_id=0 report.
            prop_oneof![
                Just(Vec::new()),
                prop::collection::vec(item_strategy(), 0..=8).prop_map(|items| vec![HidReportInfo {
                    report_id: 0,
                    items,
                }]),
            ]
            .boxed()
        }
    }

    fn collection_strategy(max_depth: u8, use_report_ids: bool) -> BoxedStrategy<HidCollectionInfo> {
        let child_strategy = if max_depth > 1 {
            prop::collection::vec(collection_strategy(max_depth - 1, use_report_ids), 0..=3).boxed()
        } else {
            Just(Vec::new()).boxed()
        };

        (
            usage_u16ish(),
            usage_u16ish(),
            collection_type_strategy(),
            report_list_strategy(use_report_ids),
            report_list_strategy(use_report_ids),
            report_list_strategy(use_report_ids),
            child_strategy,
        )
            .prop_map(
                |(
                    usage_page,
                    usage,
                    collection_type,
                    input_reports,
                    output_reports,
                    feature_reports,
                    children,
                )| HidCollectionInfo {
                    usage_page,
                    usage,
                    collection_type,
                    input_reports,
                    output_reports,
                    feature_reports,
                    children,
                },
            )
            .boxed()
    }

    fn collections_strategy() -> impl Strategy<Value = Vec<HidCollectionInfo>> {
        // Important: avoid the mixed Report ID case (some reports have id 0 while others have a
        // non-zero id) since it's invalid HID and causes ambiguity on the wire.
        any::<bool>().prop_flat_map(|use_report_ids| {
            collection_strategy(3, use_report_ids).prop_map(|root| vec![root])
        })
    }

    fn normalize_reports(reports: &mut Vec<HidReportInfo>) {
        // A report with zero main items synthesizes to nothing and cannot roundtrip.
        reports.retain(|r| !r.items.is_empty());
        for report in reports.iter_mut() {
            for item in report.items.iter_mut() {
                // `parse_report_descriptor()` expands small Usage Minimum/Maximum ranges into an
                // explicit list. Canonicalize to `[min, max]` so `is_range` items roundtrip
                // regardless of whether the range was expanded.
                if item.is_range && !item.usages.is_empty() {
                    let min = *item.usages.iter().min().expect("non-empty");
                    let max = *item.usages.iter().max().expect("non-empty");
                    item.usages = vec![min, max];
                }
            }
        }
        reports.sort_by_key(|r| r.report_id);
    }

    fn normalize_collection(collection: &mut HidCollectionInfo) {
        normalize_reports(&mut collection.input_reports);
        normalize_reports(&mut collection.output_reports);
        normalize_reports(&mut collection.feature_reports);

        for report in &mut collection.input_reports {
            for item in &mut report.items {
                normalize_item(item);
            }
        }
        for report in &mut collection.output_reports {
            for item in &mut report.items {
                normalize_item(item);
            }
        }
        for report in &mut collection.feature_reports {
            for item in &mut report.items {
                normalize_item(item);
            }
        }

        for child in &mut collection.children {
            normalize_collection(child);
        }
        collection
            .children
            .sort_by_key(|c| (c.usage_page, c.usage, c.collection_type));
    }

    fn normalize_item(item: &mut HidReportItem) {
        if item.is_range && !item.usages.is_empty() {
            let mut min = item.usages[0];
            let mut max = item.usages[0];
            for &u in &item.usages[1..] {
                min = min.min(u);
                max = max.max(u);
            }
            item.usages = vec![min, max];
        }
    }

    fn normalize_collections(collections: &mut Vec<HidCollectionInfo>) {
        for c in collections.iter_mut() {
            normalize_collection(c);
        }
        collections.sort_by_key(|c| (c.usage_page, c.usage, c.collection_type));
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            .. ProptestConfig::default()
        })]

        #[test]
        fn parse_synthesize_roundtrip(collections in collections_strategy()) {
            let bytes = synthesize_report_descriptor(&collections)
                .expect("synthesized report descriptor must succeed for generated metadata");
            let parsed = parse_report_descriptor(&bytes)
                .expect("descriptor synthesized by synthesize_report_descriptor must parse");

            let mut expected = collections.clone();
            let mut actual = parsed.collections.clone();
            normalize_collections(&mut expected);
            normalize_collections(&mut actual);
            prop_assert_eq!(actual, expected);

            // Regression safety: synthesizing a parsed descriptor must not panic or error.
            let _ = synthesize_report_descriptor(&parsed.collections).unwrap();
        }
    }
}
