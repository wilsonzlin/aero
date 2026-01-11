use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    #[error("report id {report_id} is out of range (must be <= 255)")]
    InvalidReportId { report_id: u32 },
    #[error("is_range report items must contain at least two usages (min/max)")]
    InvalidUsageRange,
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
) -> Result<Vec<HidCollectionInfo>, HidDescriptorError> {
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
                        let is_buffered_bytes = (flags & (1 << 8)) != 0;

                        let usage_page = local.usage_page_override.unwrap_or(global.usage_page);
                        let (is_range, usages) =
                            match (local.usage_minimum, local.usage_maximum) {
                                (Some(min), Some(max)) => (true, vec![min, max]),
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
                    5 => global.unit_exponent = parse_signed(data),
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
        emit_signed(out, ItemType::Global, 5, item.unit_exponent)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::io::usb::hid::{keyboard::UsbHidKeyboard, mouse::UsbHidMouse};
    use crate::io::usb::UsbDeviceModel;

    fn roundtrip(desc: &[u8]) {
        let parsed = parse_report_descriptor(desc).unwrap();
        let synthesized = synthesize_report_descriptor(&parsed).unwrap();
        let reparsed = parse_report_descriptor(&synthesized).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn roundtrip_keyboard_and_mouse() {
        let kb = UsbHidKeyboard::new();
        roundtrip(kb.get_hid_report_descriptor());

        let mouse = UsbHidMouse::new();
        roundtrip(mouse.get_hid_report_descriptor());
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
        assert_eq!(collections, reparsed);
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
        assert_eq!(collections, reparsed);
    }
}
