use serde::{Deserialize, Serialize};
use thiserror::Error;

/// JSON-compatible representation of WebHID collection metadata.
///
/// This mirrors the shape returned by the browser WebHID API (and the output of
/// `web/src/hid/webhid_normalize.ts`). The contract is locked down by cross-lang
/// fixtures under `tests/fixtures/hid/`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HidCollectionInfo {
    pub usage_page: u32,
    pub usage: u32,
    #[serde(rename = "type")]
    pub collection_type: HidCollectionType,
    pub children: Vec<HidCollectionInfo>,
    pub input_reports: Vec<HidReportInfo>,
    pub output_reports: Vec<HidReportInfo>,
    pub feature_reports: Vec<HidReportInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HidCollectionType {
    Physical,
    Application,
    Logical,
    Report,
    NamedArray,
    UsageSwitch,
    UsageModifier,
}

impl HidCollectionType {
    fn to_hid_value(self) -> u8 {
        match self {
            HidCollectionType::Physical => 0x00,
            HidCollectionType::Application => 0x01,
            HidCollectionType::Logical => 0x02,
            HidCollectionType::Report => 0x03,
            HidCollectionType::NamedArray => 0x04,
            HidCollectionType::UsageSwitch => 0x05,
            HidCollectionType::UsageModifier => 0x06,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HidReportInfo {
    pub report_id: u32,
    pub items: Vec<HidReportItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HidReportItem {
    pub usage_page: u32,
    pub usages: Vec<u32>,
    pub usage_minimum: u32,
    pub usage_maximum: u32,
    pub report_size: u32,
    pub report_count: u32,
    pub unit_exponent: i32,
    pub unit: u32,
    pub logical_minimum: i32,
    pub logical_maximum: i32,
    pub physical_minimum: i32,
    pub physical_maximum: i32,
    pub strings: Vec<u32>,
    pub string_minimum: u32,
    pub string_maximum: u32,
    pub designators: Vec<u32>,
    pub designator_minimum: u32,
    pub designator_maximum: u32,
    pub is_absolute: bool,
    pub is_array: bool,
    pub is_buffered_bytes: bool,
    pub is_constant: bool,
    pub is_linear: bool,
    pub is_range: bool,
    pub is_relative: bool,
    pub is_volatile: bool,
    pub has_null: bool,
    pub has_preferred_state: bool,
    pub is_wrapped: bool,
}

#[derive(Debug, Error)]
pub enum HidDescriptorSynthesisError {
    #[error("HID report id {report_id} is out of range (expected 0..=255)")]
    ReportIdOutOfRange { report_id: u32 },

    #[error("usage range is invalid: minimum {min} > maximum {max}")]
    InvalidUsageRange { min: u32, max: u32 },

    #[error("unsupported HID item data size: {0} bytes")]
    UnsupportedItemDataSize(usize),
}

type Result<T> = core::result::Result<T, HidDescriptorSynthesisError>;

/// Synthesize a HID report descriptor from normalized WebHID metadata.
///
/// This is intentionally minimal: it is primarily used to validate that the
/// normalized metadata format is sufficient for descriptor generation.
pub fn synthesize_report_descriptor(collections: &[HidCollectionInfo]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for collection in collections {
        write_collection(&mut out, collection)?;
    }
    Ok(out)
}

fn write_collection(out: &mut Vec<u8>, collection: &HidCollectionInfo) -> Result<()> {
    write_usage_page(out, collection.usage_page)?;
    write_usage(out, collection.usage)?;
    write_collection_start(out, collection.collection_type)?;

    for report in &collection.input_reports {
        write_report(out, report, ReportKind::Input)?;
    }
    for report in &collection.output_reports {
        write_report(out, report, ReportKind::Output)?;
    }
    for report in &collection.feature_reports {
        write_report(out, report, ReportKind::Feature)?;
    }

    for child in &collection.children {
        write_collection(out, child)?;
    }

    write_end_collection(out)?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ReportKind {
    Input,
    Output,
    Feature,
}

fn write_report(out: &mut Vec<u8>, report: &HidReportInfo, kind: ReportKind) -> Result<()> {
    if report.report_id > 0xFF {
        return Err(HidDescriptorSynthesisError::ReportIdOutOfRange {
            report_id: report.report_id,
        });
    }

    if report.report_id != 0 {
        write_report_id(out, report.report_id as u8)?;
    }

    for item in &report.items {
        write_report_item(out, item, kind)?;
    }

    Ok(())
}

fn write_report_item(out: &mut Vec<u8>, item: &HidReportItem, kind: ReportKind) -> Result<()> {
    write_usage_page(out, item.usage_page)?;

    if item.is_range {
        if item.usage_minimum > item.usage_maximum {
            return Err(HidDescriptorSynthesisError::InvalidUsageRange {
                min: item.usage_minimum,
                max: item.usage_maximum,
            });
        }
        write_usage_minimum(out, item.usage_minimum)?;
        write_usage_maximum(out, item.usage_maximum)?;
    } else {
        for usage in &item.usages {
            write_usage(out, *usage)?;
        }
    }

    write_logical_minimum(out, item.logical_minimum)?;
    write_logical_maximum(out, item.logical_maximum)?;
    write_physical_minimum(out, item.physical_minimum)?;
    write_physical_maximum(out, item.physical_maximum)?;
    write_unit_exponent(out, item.unit_exponent)?;
    write_unit(out, item.unit)?;
    write_report_size(out, item.report_size)?;
    write_report_count(out, item.report_count)?;

    let flags = report_item_flags(item);
    match kind {
        ReportKind::Input => write_input(out, flags)?,
        ReportKind::Output => write_output(out, flags)?,
        ReportKind::Feature => write_feature(out, flags)?,
    }

    Ok(())
}

fn report_item_flags(item: &HidReportItem) -> u16 {
    let mut flags: u16 = 0;

    if item.is_constant {
        flags |= 1 << 0;
    }

    // HID: bit 1 is 0 = Array, 1 = Variable.
    if !item.is_array {
        flags |= 1 << 1;
    }

    // HID: bit 2 is 0 = Absolute, 1 = Relative.
    if item.is_relative {
        flags |= 1 << 2;
    }

    // HID: bit 3 is 0 = No Wrap, 1 = Wrap.
    if item.is_wrapped {
        flags |= 1 << 3;
    }

    // HID: bit 4 is 0 = Linear, 1 = Non Linear.
    if !item.is_linear {
        flags |= 1 << 4;
    }

    // HID: bit 5 is 0 = Preferred State, 1 = No Preferred.
    if !item.has_preferred_state {
        flags |= 1 << 5;
    }

    // HID: bit 6 is 0 = No Null position, 1 = Null state.
    if item.has_null {
        flags |= 1 << 6;
    }

    // HID: bit 7 is 0 = Non Volatile, 1 = Volatile.
    if item.is_volatile {
        flags |= 1 << 7;
    }

    // HID: bit 8 is 0 = Bit Field, 1 = Buffered Bytes.
    if item.is_buffered_bytes {
        flags |= 1 << 8;
    }

    flags
}

fn encode_u32(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn encode_i32(value: i32) -> [u8; 4] {
    value.to_le_bytes()
}

fn push_short_item(out: &mut Vec<u8>, item_type: u8, tag: u8, data: &[u8]) -> Result<()> {
    let size_code = match data.len() {
        0 => 0,
        1 => 1,
        2 => 2,
        4 => 3,
        other => return Err(HidDescriptorSynthesisError::UnsupportedItemDataSize(other)),
    };
    out.push((tag << 4) | (item_type << 2) | size_code);
    out.extend_from_slice(data);
    Ok(())
}

fn push_global_u32(out: &mut Vec<u8>, tag: u8, value: u32) -> Result<()> {
    let bytes = encode_u32(value);
    let data = if value <= 0xFF {
        &bytes[..1]
    } else if value <= 0xFFFF {
        &bytes[..2]
    } else {
        &bytes[..4]
    };
    push_short_item(out, 0x01, tag, data)
}

fn push_global_i32(out: &mut Vec<u8>, tag: u8, value: i32) -> Result<()> {
    let bytes = encode_i32(value);
    let data = if (-128..=127).contains(&value) {
        &bytes[..1]
    } else if (-32768..=32767).contains(&value) {
        &bytes[..2]
    } else {
        &bytes[..4]
    };
    push_short_item(out, 0x01, tag, data)
}

fn push_local_u32(out: &mut Vec<u8>, tag: u8, value: u32) -> Result<()> {
    let bytes = encode_u32(value);
    let data = if value <= 0xFF {
        &bytes[..1]
    } else if value <= 0xFFFF {
        &bytes[..2]
    } else {
        &bytes[..4]
    };
    push_short_item(out, 0x02, tag, data)
}

fn write_usage_page(out: &mut Vec<u8>, usage_page: u32) -> Result<()> {
    push_global_u32(out, 0x00, usage_page)
}

fn write_usage(out: &mut Vec<u8>, usage: u32) -> Result<()> {
    push_local_u32(out, 0x00, usage)
}

fn write_usage_minimum(out: &mut Vec<u8>, usage: u32) -> Result<()> {
    push_local_u32(out, 0x01, usage)
}

fn write_usage_maximum(out: &mut Vec<u8>, usage: u32) -> Result<()> {
    push_local_u32(out, 0x02, usage)
}

fn write_logical_minimum(out: &mut Vec<u8>, value: i32) -> Result<()> {
    push_global_i32(out, 0x01, value)
}

fn write_logical_maximum(out: &mut Vec<u8>, value: i32) -> Result<()> {
    push_global_i32(out, 0x02, value)
}

fn write_physical_minimum(out: &mut Vec<u8>, value: i32) -> Result<()> {
    push_global_i32(out, 0x03, value)
}

fn write_physical_maximum(out: &mut Vec<u8>, value: i32) -> Result<()> {
    push_global_i32(out, 0x04, value)
}

fn write_unit_exponent(out: &mut Vec<u8>, value: i32) -> Result<()> {
    push_global_i32(out, 0x05, value)
}

fn write_unit(out: &mut Vec<u8>, value: u32) -> Result<()> {
    push_global_u32(out, 0x06, value)
}

fn write_report_size(out: &mut Vec<u8>, value: u32) -> Result<()> {
    push_global_u32(out, 0x07, value)
}

fn write_report_id(out: &mut Vec<u8>, value: u8) -> Result<()> {
    push_global_u32(out, 0x08, value as u32)
}

fn write_report_count(out: &mut Vec<u8>, value: u32) -> Result<()> {
    push_global_u32(out, 0x09, value)
}

fn write_collection_start(out: &mut Vec<u8>, kind: HidCollectionType) -> Result<()> {
    push_short_item(out, 0x00, 0x0A, &[kind.to_hid_value()])
}

fn write_end_collection(out: &mut Vec<u8>) -> Result<()> {
    push_short_item(out, 0x00, 0x0C, &[])
}

fn push_main_u16(out: &mut Vec<u8>, tag: u8, value: u16) -> Result<()> {
    if value <= 0xFF {
        push_short_item(out, 0x00, tag, &[(value & 0xFF) as u8])
    } else {
        push_short_item(out, 0x00, tag, &value.to_le_bytes())
    }
}

fn write_input(out: &mut Vec<u8>, flags: u16) -> Result<()> {
    push_main_u16(out, 0x08, flags)
}

fn write_output(out: &mut Vec<u8>, flags: u16) -> Result<()> {
    push_main_u16(out, 0x09, flags)
}

fn write_feature(out: &mut Vec<u8>, flags: u16) -> Result<()> {
    push_main_u16(out, 0x0B, flags)
}

