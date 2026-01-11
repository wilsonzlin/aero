use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::report_descriptor;

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
    pub collection_type: u8,
    pub children: Vec<HidCollectionInfo>,
    pub input_reports: Vec<HidReportInfo>,
    pub output_reports: Vec<HidReportInfo>,
    pub feature_reports: Vec<HidReportInfo>,
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

    #[error("unitExponent {unit_exponent} is out of range (expected -8..=7)")]
    UnitExponentOutOfRange { unit_exponent: i32 },

    #[error("unsupported HID item data size: {0} bytes")]
    UnsupportedItemDataSize(usize),

    #[error(transparent)]
    HidDescriptor(#[from] report_descriptor::HidDescriptorError),
}

type Result<T> = core::result::Result<T, HidDescriptorSynthesisError>;

/// Synthesize a HID report descriptor from normalized WebHID metadata.
///
/// This converts the WebHID JSON schema into the canonical WebHID-like metadata
/// used by [`crate::io::usb::hid::report_descriptor`] and then reuses the canonical
/// short-item encoder.
pub fn synthesize_report_descriptor(collections: &[HidCollectionInfo]) -> Result<Vec<u8>> {
    let converted: Vec<report_descriptor::HidCollectionInfo> =
        collections.iter().map(convert_collection).collect::<Result<_>>()?;
    Ok(report_descriptor::synthesize_report_descriptor(&converted)?)
}

fn convert_collection(
    collection: &HidCollectionInfo,
) -> Result<report_descriptor::HidCollectionInfo> {
    Ok(report_descriptor::HidCollectionInfo {
        usage_page: collection.usage_page,
        usage: collection.usage,
        collection_type: collection.collection_type,
        input_reports: collection
            .input_reports
            .iter()
            .map(convert_report)
            .collect::<Result<_>>()?,
        output_reports: collection
            .output_reports
            .iter()
            .map(convert_report)
            .collect::<Result<_>>()?,
        feature_reports: collection
            .feature_reports
            .iter()
            .map(convert_report)
            .collect::<Result<_>>()?,
        children: collection
            .children
            .iter()
            .map(convert_collection)
            .collect::<Result<_>>()?,
    })
}

fn convert_report(report: &HidReportInfo) -> Result<report_descriptor::HidReportInfo> {
    if report.report_id > 0xFF {
        return Err(HidDescriptorSynthesisError::ReportIdOutOfRange {
            report_id: report.report_id,
        });
    }

    Ok(report_descriptor::HidReportInfo {
        report_id: report.report_id,
        items: report
            .items
            .iter()
            .map(convert_item)
            .collect::<Result<_>>()?,
    })
}

fn convert_item(item: &HidReportItem) -> Result<report_descriptor::HidReportItem> {
    if !(-8..=7).contains(&item.unit_exponent) {
        return Err(HidDescriptorSynthesisError::UnitExponentOutOfRange {
            unit_exponent: item.unit_exponent,
        });
    }

    let usages = if item.is_range {
        if item.usage_minimum > item.usage_maximum {
            return Err(HidDescriptorSynthesisError::InvalidUsageRange {
                min: item.usage_minimum,
                max: item.usage_maximum,
            });
        }
        vec![item.usage_minimum, item.usage_maximum]
    } else {
        item.usages.clone()
    };

    Ok(report_descriptor::HidReportItem {
        is_array: item.is_array,
        is_absolute: item.is_absolute,
        is_buffered_bytes: item.is_buffered_bytes,
        is_constant: item.is_constant,
        is_range: item.is_range,
        logical_minimum: item.logical_minimum,
        logical_maximum: item.logical_maximum,
        physical_minimum: item.physical_minimum,
        physical_maximum: item.physical_maximum,
        unit_exponent: item.unit_exponent,
        unit: item.unit,
        report_size: item.report_size,
        report_count: item.report_count,
        usage_page: item.usage_page,
        usages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(unit_exponent: i32) -> HidReportItem {
        HidReportItem {
            usage_page: 0x01,
            usages: vec![0x30],
            usage_minimum: 0,
            usage_maximum: 0,
            report_size: 8,
            report_count: 1,
            unit_exponent,
            unit: 0,
            logical_minimum: 0,
            logical_maximum: 127,
            physical_minimum: 0,
            physical_maximum: 0,
            strings: vec![],
            string_minimum: 0,
            string_maximum: 0,
            designators: vec![],
            designator_minimum: 0,
            designator_maximum: 0,
            is_absolute: true,
            is_array: false,
            is_buffered_bytes: false,
            is_constant: false,
            is_linear: true,
            is_range: false,
            is_relative: false,
            is_volatile: false,
            has_null: false,
            has_preferred_state: true,
            is_wrapped: false,
        }
    }

    fn make_collections(item: HidReportItem) -> Vec<HidCollectionInfo> {
        vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: 0x01,
            children: vec![],
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![item],
            }],
            output_reports: vec![],
            feature_reports: vec![],
        }]
    }

    #[test]
    fn unit_exponent_encodes_as_4bit_signed_nibble() {
        let collections = make_collections(make_item(-1));

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
    fn unit_exponent_out_of_range_is_rejected() {
        let collections = make_collections(make_item(8));

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorSynthesisError::UnitExponentOutOfRange { unit_exponent }) => {
                assert_eq!(unit_exponent, 8);
            }
            other => panic!("expected UnitExponentOutOfRange error, got {other:?}"),
        }
    }
}
