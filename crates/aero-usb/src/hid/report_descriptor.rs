//! HID report descriptor <-> WebHID-style representation.
//!
//! WebHID exposes parsed report items (see `HIDReportItem`) where `isRange == true`
//! means `usages` contains the expanded list of usages covered by the range.
//! For example, keyboard modifiers are described as the usage range `0xE0..=0xE7`
//! but WebHID returns `usages = [0xE0, 0xE1, ..., 0xE7]`.
//!
//! This module parses raw HID report descriptors into a representation that matches
//! that expectation, and synthesizes report descriptors from that representation.
//!
//! The parser/synthesizer intentionally focuses on the HID items we need today
//! (Usage Page, Usage/Usage Min/Max, Report Size/Count/ID, Logical Min/Max,
//! Collection, Input/Output/Feature). It is not a complete HID descriptor
//! implementation.

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

/// Inclusive usage ranges larger than this are not expanded to avoid huge allocations.
pub const MAX_EXPANDED_USAGE_RANGE: u32 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportDescriptorParseResult {
    pub collections: Vec<HidCollection>,
    /// True when the parser encountered at least one large `Usage Minimum/Maximum`
    /// range that was not expanded.
    pub truncated_ranges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidCollection {
    pub usage_page: u32,
    pub usages: Vec<u32>,
    pub is_range: bool,
    pub collection_type: u8,
    pub children: Vec<HidCollection>,
    pub input_reports: Vec<HidReportInfo>,
    pub output_reports: Vec<HidReportInfo>,
    pub feature_reports: Vec<HidReportInfo>,
}

impl HidCollection {
    fn empty() -> Self {
        Self {
            usage_page: 0,
            usages: Vec::new(),
            is_range: false,
            collection_type: 0,
            children: Vec::new(),
            input_reports: Vec::new(),
            output_reports: Vec::new(),
            feature_reports: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidReportInfo {
    pub report_id: u8,
    pub items: Vec<HidReportItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidMainItemKind {
    Input,
    Output,
    Feature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidReportItem {
    pub kind: HidMainItemKind,
    /// Raw main-item flags (Input/Output/Feature data bits).
    pub flags: u32,
    pub usage_page: u32,
    pub usages: Vec<u32>,
    pub is_range: bool,
    pub logical_minimum: i32,
    pub logical_maximum: i32,
    pub report_size: u32,
    pub report_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportDescriptorError {
    UnexpectedEnd,
    UnsupportedLongItem,
    InvalidItemSize { prefix: u8, size: usize },
    GlobalStackUnderflow,
    CollectionStackUnderflow,
    UnclosedCollections { remaining: usize },
    IncompleteUsageRange,
    UsageRangeMinGreaterThanMax { min: u32, max: u32 },
    MixedReportIdUsage,
    ReportIdOutOfRange(u32),
}

impl fmt::Display for ReportDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => write!(f, "unexpected end of HID report descriptor"),
            Self::UnsupportedLongItem => write!(f, "unsupported HID long item"),
            Self::InvalidItemSize { prefix, size } => {
                write!(f, "invalid HID item size {size} (prefix 0x{prefix:02X})")
            }
            Self::GlobalStackUnderflow => write!(f, "HID global item stack underflow"),
            Self::CollectionStackUnderflow => write!(f, "HID collection stack underflow"),
            Self::UnclosedCollections { remaining } => {
                write!(f, "HID report descriptor has {remaining} unclosed collection(s)")
            }
            Self::IncompleteUsageRange => write!(f, "incomplete HID usage range (missing min/max)"),
            Self::UsageRangeMinGreaterThanMax { min, max } => {
                write!(f, "invalid HID usage range: min ({min}) > max ({max})")
            }
            Self::MixedReportIdUsage => write!(
                f,
                "invalid report IDs: mixed report_id==0 and report_id!=0 in the same descriptor"
            ),
            Self::ReportIdOutOfRange(v) => write!(f, "report id out of range for u8: {v}"),
        }
    }
}

impl std::error::Error for ReportDescriptorError {}

#[derive(Clone, Debug)]
struct GlobalState {
    usage_page: u32,
    logical_minimum: i32,
    logical_maximum: i32,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self {
            usage_page: 0,
            logical_minimum: 0,
            logical_maximum: 0,
            report_size: 0,
            report_count: 0,
            report_id: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LocalState {
    usages: Vec<u32>,
    usage_min: Option<u32>,
    usage_max: Option<u32>,
}

impl LocalState {
    fn reset(&mut self) {
        self.usages.clear();
        self.usage_min = None;
        self.usage_max = None;
    }
}

pub fn parse_report_descriptor(
    bytes: &[u8],
) -> Result<ReportDescriptorParseResult, ReportDescriptorError> {
    let mut i = 0usize;
    let mut globals = GlobalState::default();
    let mut global_stack: Vec<GlobalState> = Vec::new();
    let mut locals = LocalState::default();
    let mut truncated_ranges = false;

    let mut collection_stack: Vec<HidCollection> = Vec::new();
    collection_stack.push(HidCollection::empty()); // synthetic root

    while i < bytes.len() {
        let prefix = bytes[i];
        i += 1;

        if prefix == 0xFE {
            return Err(ReportDescriptorError::UnsupportedLongItem);
        }

        let size_code = prefix & 0x03;
        let size = match size_code {
            0 => 0usize,
            1 => 1usize,
            2 => 2usize,
            3 => 4usize,
            _ => unreachable!(),
        };

        if i + size > bytes.len() {
            return Err(ReportDescriptorError::UnexpectedEnd);
        }
        let data = &bytes[i..i + size];
        i += size;

        let item_type = (prefix >> 2) & 0x03;
        let tag = (prefix >> 4) & 0x0F;

        match item_type {
            0x00 => {
                // Main
                match tag {
                    0x08 | 0x09 | 0x0B => {
                        // Input/Output/Feature
                        let kind = match tag {
                            0x08 => HidMainItemKind::Input,
                            0x09 => HidMainItemKind::Output,
                            0x0B => HidMainItemKind::Feature,
                            _ => unreachable!(),
                        };
                        let flags = decode_u32_le(data)?;
                        let (usages, is_range) =
                            take_local_usages(&locals, &mut truncated_ranges)?;
                        locals.reset();

                        let item = HidReportItem {
                            kind,
                            flags,
                            usage_page: globals.usage_page,
                            usages,
                            is_range,
                            logical_minimum: globals.logical_minimum,
                            logical_maximum: globals.logical_maximum,
                            report_size: globals.report_size,
                            report_count: globals.report_count,
                        };

                        let cur = collection_stack
                            .last_mut()
                            .ok_or(ReportDescriptorError::CollectionStackUnderflow)?;
                        let reports = match kind {
                            HidMainItemKind::Input => &mut cur.input_reports,
                            HidMainItemKind::Output => &mut cur.output_reports,
                            HidMainItemKind::Feature => &mut cur.feature_reports,
                        };
                        push_report_item(reports, globals.report_id, item);
                    }
                    0x0A => {
                        // Collection
                        if data.len() != 1 {
                            return Err(ReportDescriptorError::InvalidItemSize {
                                prefix,
                                size: data.len(),
                            });
                        }
                        let collection_type = data[0];
                        let (usages, is_range) =
                            take_local_usages(&locals, &mut truncated_ranges)?;
                        locals.reset();

                        let mut collection = HidCollection::empty();
                        collection.usage_page = globals.usage_page;
                        collection.usages = usages;
                        collection.is_range = is_range;
                        collection.collection_type = collection_type;
                        collection_stack.push(collection);
                    }
                    0x0C => {
                        // End Collection
                        if data.len() != 0 {
                            return Err(ReportDescriptorError::InvalidItemSize {
                                prefix,
                                size: data.len(),
                            });
                        }
                        let finished = collection_stack
                            .pop()
                            .ok_or(ReportDescriptorError::CollectionStackUnderflow)?;
                        let parent = collection_stack
                            .last_mut()
                            .ok_or(ReportDescriptorError::CollectionStackUnderflow)?;
                        parent.children.push(finished);
                        locals.reset();
                    }
                    _ => {
                        // Unknown main item: ignore but still clears locals per HID rules.
                        locals.reset();
                    }
                }
            }
            0x01 => {
                // Global
                match tag {
                    0x00 => globals.usage_page = decode_u32_le(data)?,
                    0x01 => globals.logical_minimum = decode_i32_le(data)?,
                    0x02 => globals.logical_maximum = decode_i32_le(data)?,
                    0x07 => globals.report_size = decode_u32_le(data)?,
                    0x08 => {
                        let v = decode_u32_le(data)?;
                        let Some(v8) = u8::try_from(v).ok() else {
                            return Err(ReportDescriptorError::ReportIdOutOfRange(v));
                        };
                        globals.report_id = v8;
                    }
                    0x09 => globals.report_count = decode_u32_le(data)?,
                    0x0A => {
                        // Push
                        global_stack.push(globals.clone());
                    }
                    0x0B => {
                        // Pop
                        globals = global_stack
                            .pop()
                            .ok_or(ReportDescriptorError::GlobalStackUnderflow)?;
                    }
                    _ => {}
                }
            }
            0x02 => {
                // Local
                match tag {
                    0x00 => locals.usages.push(decode_u32_le(data)?),
                    0x01 => locals.usage_min = Some(decode_u32_le(data)?),
                    0x02 => locals.usage_max = Some(decode_u32_le(data)?),
                    _ => {}
                }
            }
            _ => {
                // Reserved
            }
        }
    }

    if collection_stack.len() != 1 {
        return Err(ReportDescriptorError::UnclosedCollections {
            remaining: collection_stack.len() - 1,
        });
    }

    let root = collection_stack.pop().unwrap();

    Ok(ReportDescriptorParseResult {
        collections: root.children,
        truncated_ranges,
    })
}

fn push_report_item(reports: &mut Vec<HidReportInfo>, report_id: u8, item: HidReportItem) {
    if let Some(info) = reports.iter_mut().find(|r| r.report_id == report_id) {
        info.items.push(item);
        return;
    }
    reports.push(HidReportInfo {
        report_id,
        items: vec![item],
    });
}

fn take_local_usages(
    locals: &LocalState,
    truncated_ranges: &mut bool,
) -> Result<(Vec<u32>, bool), ReportDescriptorError> {
    if locals.usage_min.is_some() || locals.usage_max.is_some() {
        let Some(min) = locals.usage_min else {
            return Err(ReportDescriptorError::IncompleteUsageRange);
        };
        let Some(max) = locals.usage_max else {
            return Err(ReportDescriptorError::IncompleteUsageRange);
        };
        if min > max {
            return Err(ReportDescriptorError::UsageRangeMinGreaterThanMax { min, max });
        }
        let Some(len) = max.checked_sub(min).and_then(|d| d.checked_add(1)) else {
            // Overflow (should be impossible for well-formed usage values).
            *truncated_ranges = true;
            return Ok((vec![min, max], true));
        };
        if len > MAX_EXPANDED_USAGE_RANGE {
            *truncated_ranges = true;
            return Ok((vec![min, max], true));
        }
        let mut out = Vec::with_capacity(len as usize);
        for u in min..=max {
            out.push(u);
        }
        return Ok((out, true));
    }

    Ok((locals.usages.clone(), false))
}

pub fn synthesize_report_descriptor(
    collections: &[HidCollection],
) -> Result<Vec<u8>, ReportDescriptorError> {
    validate_report_ids(collections)?;

    let mut out = Vec::new();
    for c in collections {
        emit_collection(&mut out, c, None)?;
    }
    Ok(out)
}

fn validate_report_ids(collections: &[HidCollection]) -> Result<(), ReportDescriptorError> {
    fn walk_collection(
        c: &HidCollection,
        has_zero: &mut bool,
        has_nonzero: &mut bool,
    ) {
        for report in c
            .input_reports
            .iter()
            .chain(c.output_reports.iter())
            .chain(c.feature_reports.iter())
        {
            if report.report_id == 0 {
                *has_zero = true;
            } else {
                *has_nonzero = true;
            }
        }
        for child in &c.children {
            walk_collection(child, has_zero, has_nonzero);
        }
    }

    let mut has_zero = false;
    let mut has_nonzero = false;
    for c in collections {
        walk_collection(c, &mut has_zero, &mut has_nonzero);
    }
    if has_zero && has_nonzero {
        return Err(ReportDescriptorError::MixedReportIdUsage);
    }
    Ok(())
}

fn emit_collection(
    out: &mut Vec<u8>,
    c: &HidCollection,
    active_report_id: Option<u8>,
) -> Result<Option<u8>, ReportDescriptorError> {
    // Usage Page for collection usage.
    if c.usage_page != 0 {
        emit_u32(out, ItemType::Global, 0x00, c.usage_page)?;
    }

    emit_usages(out, c.is_range, &c.usages)?;
    emit_u8(out, ItemType::Main, 0x0A, c.collection_type)?;

    let mut report_id = active_report_id;

    // Reports. Ordering is deterministic: input, output, feature.
    report_id = emit_reports(out, &c.input_reports, HidMainItemKind::Input, report_id)?;
    report_id = emit_reports(out, &c.output_reports, HidMainItemKind::Output, report_id)?;
    report_id = emit_reports(out, &c.feature_reports, HidMainItemKind::Feature, report_id)?;

    // Child collections.
    for child in &c.children {
        report_id = emit_collection(out, child, report_id)?;
    }

    emit_item(out, ItemType::Main, 0x0C, &[])?;
    Ok(report_id)
}

fn emit_reports(
    out: &mut Vec<u8>,
    reports: &[HidReportInfo],
    kind: HidMainItemKind,
    mut active_report_id: Option<u8>,
) -> Result<Option<u8>, ReportDescriptorError> {
    for report in reports {
        let rid = report.report_id;
        if rid != 0 && active_report_id != Some(rid) {
            emit_u8(out, ItemType::Global, 0x08, rid)?;
            active_report_id = Some(rid);
        }
        for item in &report.items {
            // Usage Page for the report item.
            if item.usage_page != 0 {
                emit_u32(out, ItemType::Global, 0x00, item.usage_page)?;
            }
            emit_i32(out, ItemType::Global, 0x01, item.logical_minimum)?;
            emit_i32(out, ItemType::Global, 0x02, item.logical_maximum)?;
            emit_u32(out, ItemType::Global, 0x07, item.report_size)?;
            emit_u32(out, ItemType::Global, 0x09, item.report_count)?;

            let item_kind = item.kind;
            let expected_kind = kind;
            debug_assert_eq!(item_kind, expected_kind);

            emit_usages(out, item.is_range, &item.usages)?;
            let tag = match item_kind {
                HidMainItemKind::Input => 0x08,
                HidMainItemKind::Output => 0x09,
                HidMainItemKind::Feature => 0x0B,
            };
            emit_u32(out, ItemType::Main, tag, item.flags)?;
        }
    }
    Ok(active_report_id)
}

fn emit_usages(out: &mut Vec<u8>, is_range: bool, usages: &[u32]) -> Result<(), ReportDescriptorError> {
    if !is_range {
        for &u in usages {
            emit_u32(out, ItemType::Local, 0x00, u)?;
        }
        return Ok(());
    }

    if usages.is_empty() {
        return Ok(());
    }

    let (min, max, contiguous) = range_properties(usages)?;
    if contiguous {
        emit_u32(out, ItemType::Local, 0x01, min)?;
        emit_u32(out, ItemType::Local, 0x02, max)?;
        return Ok(());
    }

    // Non-contiguous: fall back to explicit usages (sorted ascending).
    let mut sorted = usages.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for u in sorted {
        emit_u32(out, ItemType::Local, 0x00, u)?;
    }
    Ok(())
}

fn range_properties(usages: &[u32]) -> Result<(u32, u32, bool), ReportDescriptorError> {
    let mut sorted = usages.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let Some(&min) = sorted.first() else {
        return Ok((0, 0, true));
    };
    let max = *sorted.last().unwrap();

    if min == max {
        return Ok((min, max, true));
    }

    // Support both WebHID-style expanded lists and the legacy `[min, max]` representation.
    if sorted.len() == 2 {
        return Ok((min, max, true));
    }

    let Some(span_len) = max.checked_sub(min).and_then(|d| d.checked_add(1)) else {
        return Ok((min, max, false));
    };
    if sorted.len() as u32 != span_len {
        return Ok((min, max, false));
    }
    for (idx, val) in sorted.iter().enumerate() {
        if *val != min + (idx as u32) {
            return Ok((min, max, false));
        }
    }
    Ok((min, max, true))
}

#[derive(Debug, Clone, Copy)]
enum ItemType {
    Main = 0x00,
    Global = 0x01,
    Local = 0x02,
}

fn emit_u8(out: &mut Vec<u8>, item_type: ItemType, tag: u8, value: u8) -> Result<(), ReportDescriptorError> {
    emit_item(out, item_type, tag, &[value])
}

fn emit_u32(out: &mut Vec<u8>, item_type: ItemType, tag: u8, value: u32) -> Result<(), ReportDescriptorError> {
    let data = encode_u32(value);
    emit_item(out, item_type, tag, &data)
}

fn emit_i32(out: &mut Vec<u8>, item_type: ItemType, tag: u8, value: i32) -> Result<(), ReportDescriptorError> {
    let data = encode_i32(value);
    emit_item(out, item_type, tag, &data)
}

fn emit_item(out: &mut Vec<u8>, item_type: ItemType, tag: u8, data: &[u8]) -> Result<(), ReportDescriptorError> {
    let size_code = match data.len() {
        0 => 0u8,
        1 => 1u8,
        2 => 2u8,
        4 => 3u8,
        other => {
            return Err(ReportDescriptorError::InvalidItemSize {
                prefix: 0,
                size: other,
            })
        }
    };
    let prefix = (tag << 4) | ((item_type as u8) << 2) | size_code;
    out.push(prefix);
    out.extend_from_slice(data);
    Ok(())
}

fn decode_u32_le(data: &[u8]) -> Result<u32, ReportDescriptorError> {
    match data.len() {
        0 => Ok(0),
        1 => Ok(data[0] as u32),
        2 => Ok(u16::from_le_bytes([data[0], data[1]]) as u32),
        4 => Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]])),
        other => Err(ReportDescriptorError::InvalidItemSize {
            prefix: 0,
            size: other,
        }),
    }
}

fn decode_i32_le(data: &[u8]) -> Result<i32, ReportDescriptorError> {
    match data.len() {
        0 => Ok(0),
        1 => Ok(i8::from_le_bytes([data[0]]) as i32),
        2 => Ok(i16::from_le_bytes([data[0], data[1]]) as i32),
        4 => Ok(i32::from_le_bytes([data[0], data[1], data[2], data[3]])),
        other => Err(ReportDescriptorError::InvalidItemSize {
            prefix: 0,
            size: other,
        }),
    }
}

fn encode_u32(value: u32) -> Vec<u8> {
    if value <= 0xFF {
        return vec![value as u8];
    }
    if value <= 0xFFFF {
        return (value as u16).to_le_bytes().to_vec();
    }
    value.to_le_bytes().to_vec()
}

fn encode_i32(value: i32) -> Vec<u8> {
    if let Ok(v) = i8::try_from(value) {
        return vec![v as u8];
    }
    if let Ok(v) = i16::try_from(value) {
        return v.to_le_bytes().to_vec();
    }
    value.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYBOARD_DESCRIPTOR: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x06, // Usage (Keyboard)
        0xA1, 0x01, // Collection (Application)
        0x05, 0x07, // Usage Page (Keyboard)
        0x19, 0xE0, // Usage Minimum (Left Control)
        0x29, 0xE7, // Usage Maximum (Right GUI)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x75, 0x01, // Report Size (1)
        0x95, 0x08, // Report Count (8)
        0x81, 0x02, // Input (Data, Variable, Absolute)
        0x95, 0x01, // Report Count (1)
        0x75, 0x08, // Report Size (8)
        0x81, 0x01, // Input (Constant)
        0x95, 0x06, // Report Count (6)
        0x75, 0x08, // Report Size (8)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x65, // Logical Maximum (101)
        0x05, 0x07, // Usage Page (Keyboard)
        0x19, 0x00, // Usage Minimum (0)
        0x29, 0x65, // Usage Maximum (101)
        0x81, 0x00, // Input (Data, Array)
        0xC0, // End Collection
    ];

    fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn keyboard_modifier_usage_range_expands_and_synthesizes() {
        let parsed = parse_report_descriptor(KEYBOARD_DESCRIPTOR).unwrap();
        assert!(!parsed.truncated_ranges);
        assert_eq!(parsed.collections.len(), 1);

        let collection = &parsed.collections[0];
        assert_eq!(collection.input_reports.len(), 1);
        let report = &collection.input_reports[0];
        assert_eq!(report.report_id, 0);
        assert!(report.items.len() >= 1);

        let modifier_item = &report.items[0];
        assert!(modifier_item.is_range);
        assert_eq!(
            modifier_item.usages,
            vec![0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7]
        );

        let synthesized = synthesize_report_descriptor(&parsed.collections).unwrap();
        assert!(
            bytes_contains(&synthesized, &[0x19, 0xE0, 0x29, 0xE7]),
            "expected Usage Minimum/Maximum to cover E0..E7, got: {synthesized:02X?}"
        );
    }

    #[test]
    fn synth_range_single_usage_emits_min_eq_max() {
        let collections = vec![HidCollection {
            usage_page: 0x01,
            usages: vec![0x00],
            is_range: false,
            collection_type: 0x01,
            children: Vec::new(),
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    kind: HidMainItemKind::Input,
                    flags: 0x02,
                    usage_page: 0x07,
                    usages: vec![5],
                    is_range: true,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    report_size: 1,
                    report_count: 1,
                }],
            }],
            output_reports: Vec::new(),
            feature_reports: Vec::new(),
        }];

        let bytes = synthesize_report_descriptor(&collections).unwrap();
        assert!(bytes_contains(&bytes, &[0x19, 0x05, 0x29, 0x05]));
    }

    #[test]
    fn synth_noncontiguous_range_falls_back_to_explicit_usages() {
        let collections = vec![HidCollection {
            usage_page: 0x01,
            usages: vec![0x00],
            is_range: false,
            collection_type: 0x01,
            children: Vec::new(),
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![HidReportItem {
                    kind: HidMainItemKind::Input,
                    flags: 0x02,
                    usage_page: 0x07,
                    usages: vec![1, 3, 4],
                    is_range: true,
                    logical_minimum: 0,
                    logical_maximum: 1,
                    report_size: 1,
                    report_count: 3,
                }],
            }],
            output_reports: Vec::new(),
            feature_reports: Vec::new(),
        }];

        let bytes = synthesize_report_descriptor(&collections).unwrap();
        assert!(bytes_contains(&bytes, &[0x09, 0x01, 0x09, 0x03, 0x09, 0x04]));
        assert!(!bytes_contains(&bytes, &[0x19, 0x01, 0x29, 0x04]));
    }
}
