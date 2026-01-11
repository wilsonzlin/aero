use serde::de::{Error as DeError, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::report_descriptor;
fn default_true() -> bool {
    true
}

const MAX_EXPLICIT_USAGES: usize = 4096;
const MAX_RANGE_CONTIGUITY_CHECK_LEN: usize = 4096;

/// JSON-compatible representation of WebHID collection metadata.
///
/// This is the normalized metadata contract derived from the browser WebHID API (see
/// `web/src/hid/webhid_normalize.ts`). The contract is locked down by cross-lang fixtures under
/// `tests/fixtures/hid/`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HidCollectionInfo {
    pub usage_page: u32,
    pub usage: u32,
    // Normalized WebHID metadata uses a numeric `collectionType` code (0..=6) that matches the
    // HID report descriptor `Collection(...)` payload. For resilience we also accept the WebHID
    // string enum form under the `type` field.
    #[serde(alias = "type")]
    pub collection_type: HidCollectionType,
    pub children: Vec<HidCollectionInfo>,
    pub input_reports: Vec<HidReportInfo>,
    pub output_reports: Vec<HidReportInfo>,
    pub feature_reports: Vec<HidReportInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HidCollectionType {
    Physical = 0x00,
    Application = 0x01,
    Logical = 0x02,
    Report = 0x03,
    NamedArray = 0x04,
    UsageSwitch = 0x05,
    UsageModifier = 0x06,
}

impl HidCollectionType {
    pub const fn code(self) -> u8 {
        self as u8
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0x00 => Some(HidCollectionType::Physical),
            0x01 => Some(HidCollectionType::Application),
            0x02 => Some(HidCollectionType::Logical),
            0x03 => Some(HidCollectionType::Report),
            0x04 => Some(HidCollectionType::NamedArray),
            0x05 => Some(HidCollectionType::UsageSwitch),
            0x06 => Some(HidCollectionType::UsageModifier),
            _ => None,
        }
    }
}

impl Serialize for HidCollectionType {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for HidCollectionType {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HidCollectionTypeVisitor;

        impl<'de> Visitor<'de> for HidCollectionTypeVisitor {
            type Value = HidCollectionType;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str("a HID collection type (string enum or numeric code 0..=6)")
            }

            fn visit_u64<E>(self, value: u64) -> core::result::Result<Self::Value, E>
            where
                E: DeError,
            {
                let code = u8::try_from(value)
                    .map_err(|_| E::invalid_value(Unexpected::Unsigned(value), &self))?;
                HidCollectionType::from_code(code)
                    .ok_or_else(|| E::invalid_value(Unexpected::Unsigned(u64::from(code)), &self))
            }

            fn visit_i64<E>(self, value: i64) -> core::result::Result<Self::Value, E>
            where
                E: DeError,
            {
                if value < 0 {
                    return Err(E::invalid_value(Unexpected::Signed(value), &self));
                }
                self.visit_u64(value as u64)
            }

            fn visit_str<E>(self, value: &str) -> core::result::Result<Self::Value, E>
            where
                E: DeError,
            {
                match value {
                    "physical" => Ok(HidCollectionType::Physical),
                    "application" => Ok(HidCollectionType::Application),
                    "logical" => Ok(HidCollectionType::Logical),
                    "report" => Ok(HidCollectionType::Report),
                    "namedArray" => Ok(HidCollectionType::NamedArray),
                    "usageSwitch" => Ok(HidCollectionType::UsageSwitch),
                    "usageModifier" => Ok(HidCollectionType::UsageModifier),
                    other => Err(E::invalid_value(Unexpected::Str(other), &self)),
                }
            }

            fn visit_string<E>(self, value: String) -> core::result::Result<Self::Value, E>
            where
                E: DeError,
            {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_any(HidCollectionTypeVisitor)
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

    // Boolean properties surfaced by WebHID.
    //
    // These correspond to HID main-item (Input/Output/Feature) flag bits 0..=8.
    // See `docs/webhid-hid-report-descriptor-synthesis.md` for the exact bit mapping.
    pub is_absolute: bool,
    pub is_array: bool,
    pub is_buffered_bytes: bool,
    pub is_constant: bool,
    #[serde(default = "default_true")]
    pub is_linear: bool,
    pub is_range: bool,
    pub is_relative: bool,
    #[serde(default)]
    pub is_volatile: bool,
    #[serde(default)]
    pub has_null: bool,
    #[serde(default = "default_true")]
    pub has_preferred_state: bool,
    #[serde(default)]
    pub is_wrapped: bool,
}

#[derive(Debug, Clone, Copy)]
enum HidReportKindPath {
    Input,
    Output,
    Feature,
}

impl HidReportKindPath {
    const fn as_str(self) -> &'static str {
        match self {
            HidReportKindPath::Input => "inputReports",
            HidReportKindPath::Output => "outputReports",
            HidReportKindPath::Feature => "featureReports",
        }
    }
}

#[derive(Debug, Clone)]
enum HidMetadataPathSegment {
    Collections(usize),
    Children(usize),
    Report {
        kind: HidReportKindPath,
        index: usize,
        report_id: u32,
    },
    Items(usize),
}

/// Location within a WebHID metadata tree, used for pathful error reporting.
///
/// This is *not* a JSON pointer; it is a stable, index-based path that is resilient to field
/// reordering and can be embedded into errors.
#[derive(Debug, Default)]
struct HidMetadataPath {
    segments: Vec<HidMetadataPathSegment>,
}

impl HidMetadataPath {
    fn push(&mut self, seg: HidMetadataPathSegment) {
        self.segments.push(seg);
    }

    fn pop(&mut self) {
        self.segments.pop();
    }

    fn push_collection(&mut self, index: usize) {
        self.push(HidMetadataPathSegment::Collections(index));
    }

    fn push_child(&mut self, index: usize) {
        self.push(HidMetadataPathSegment::Children(index));
    }

    fn push_report(&mut self, kind: HidReportKindPath, index: usize, report_id: u32) {
        self.push(HidMetadataPathSegment::Report {
            kind,
            index,
            report_id,
        });
    }

    fn push_item(&mut self, index: usize) {
        self.push(HidMetadataPathSegment::Items(index));
    }
}

impl core::fmt::Display for HidMetadataPath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (idx, seg) in self.segments.iter().enumerate() {
            if idx != 0 {
                f.write_str("/")?;
            }
            match seg {
                HidMetadataPathSegment::Collections(index) => write!(f, "collections[{index}]")?,
                HidMetadataPathSegment::Children(index) => write!(f, "children[{index}]")?,
                HidMetadataPathSegment::Report {
                    kind,
                    index,
                    report_id,
                } => write!(f, "{}[{index}](reportId={report_id})", kind.as_str())?,
                HidMetadataPathSegment::Items(index) => write!(f, "items[{index}]")?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct SynthesisValidationContext {
    first_zero_report: Option<String>,
    first_non_zero_report: Option<String>,
}

#[derive(Debug, Error)]
pub enum HidDescriptorSynthesisError {
    #[error("{path}: HID report id {report_id} is out of range (expected 0..=255)")]
    ReportIdOutOfRange { path: String, report_id: u32 },

    #[error("mixed report IDs are not allowed (reportId=0 at {zero_path}, non-zero reportId at {non_zero_path})")]
    MixedReportIds {
        zero_path: String,
        non_zero_path: String,
    },

    #[error("{path}: usage range is invalid: minimum {min} > maximum {max}")]
    InvalidUsageRange { path: String, min: u32, max: u32 },

    #[error("{path}: usages list is too long ({len} > {max})")]
    UsagesTooLong {
        path: String,
        len: usize,
        max: usize,
    },

    #[error("{path}: isRange=true but usages list is not a contiguous range")]
    InvalidRangeUsages { path: String },

    #[error("{path}: unitExponent {unit_exponent} is out of range (expected -8..=7)")]
    UnitExponentOutOfRange { path: String, unit_exponent: i32 },

    #[error("{path}: reportSize must be non-zero")]
    ReportSizeZero { path: String },

    #[error("{path}: reportSize * reportCount overflows u32 ({report_size} * {report_count})")]
    ReportBitLengthOverflow {
        path: String,
        report_size: u32,
        report_count: u32,
    },

    #[error("{path}: {field} value {value} is out of range (expected 0..=0xFFFF)")]
    ValueOutOfU16Range {
        path: String,
        field: &'static str,
        value: u32,
    },

    #[error(
        "HID descriptor synthesis failed{path_context}: {source}",
        path_context = path.as_ref().map(|p| format!(" at {p}")).unwrap_or_default()
    )]
    HidDescriptor {
        path: Option<String>,
        #[source]
        source: report_descriptor::HidDescriptorError,
    },
}

type Result<T> = core::result::Result<T, HidDescriptorSynthesisError>;

impl From<report_descriptor::HidDescriptorError> for HidDescriptorSynthesisError {
    fn from(source: report_descriptor::HidDescriptorError) -> Self {
        HidDescriptorSynthesisError::HidDescriptor { path: None, source }
    }
}

/// Synthesize a HID report descriptor from normalized WebHID metadata.
///
/// This converts the WebHID JSON schema into the canonical WebHID-like metadata
/// used by [`crate::io::usb::hid::report_descriptor`] and then reuses the canonical
/// short-item encoder.
pub fn synthesize_report_descriptor(collections: &[HidCollectionInfo]) -> Result<Vec<u8>> {
    let mut ctx = SynthesisValidationContext::default();
    let mut path = HidMetadataPath::default();

    let mut converted = Vec::with_capacity(collections.len());
    for (idx, collection) in collections.iter().enumerate() {
        path.push_collection(idx);
        converted.push(convert_collection(collection, &mut path, &mut ctx)?);
        path.pop();
    }
    Ok(report_descriptor::synthesize_report_descriptor(&converted)?)
}

fn convert_collection(
    collection: &HidCollectionInfo,
    path: &mut HidMetadataPath,
    ctx: &mut SynthesisValidationContext,
) -> Result<report_descriptor::HidCollectionInfo> {
    validate_u16_range(collection.usage_page, "usagePage", path)?;
    validate_u16_range(collection.usage, "usage", path)?;

    Ok(report_descriptor::HidCollectionInfo {
        usage_page: collection.usage_page,
        usage: collection.usage,
        collection_type: collection.collection_type.code(),
        input_reports: collection
            .input_reports
            .iter()
            .enumerate()
            .map(|(idx, report)| {
                path.push_report(HidReportKindPath::Input, idx, report.report_id);
                let converted = convert_report(HidReportKindPath::Input, report, path, ctx);
                path.pop();
                converted
            })
            .collect::<Result<_>>()?,
        output_reports: collection
            .output_reports
            .iter()
            .enumerate()
            .map(|(idx, report)| {
                path.push_report(HidReportKindPath::Output, idx, report.report_id);
                let converted = convert_report(HidReportKindPath::Output, report, path, ctx);
                path.pop();
                converted
            })
            .collect::<Result<_>>()?,
        feature_reports: collection
            .feature_reports
            .iter()
            .enumerate()
            .map(|(idx, report)| {
                path.push_report(HidReportKindPath::Feature, idx, report.report_id);
                let converted = convert_report(HidReportKindPath::Feature, report, path, ctx);
                path.pop();
                converted
            })
            .collect::<Result<_>>()?,
        children: collection
            .children
            .iter()
            .enumerate()
            .map(|(idx, child)| {
                path.push_child(idx);
                let converted = convert_collection(child, path, ctx);
                path.pop();
                converted
            })
            .collect::<Result<_>>()?,
    })
}

fn validate_u16_range(value: u32, field: &'static str, path: &HidMetadataPath) -> Result<()> {
    if value > 0xFFFF {
        return Err(HidDescriptorSynthesisError::ValueOutOfU16Range {
            path: path.to_string(),
            field,
            value,
        });
    }
    Ok(())
}

fn validate_mixed_report_ids(
    report_id: u32,
    path: &HidMetadataPath,
    ctx: &mut SynthesisValidationContext,
) -> Result<()> {
    if report_id == 0 {
        // Only format the path string when we need to store or report it; a descriptor can have a
        // large number of reports, so avoid allocating on every visit.
        if ctx.first_zero_report.is_none() || ctx.first_non_zero_report.is_some() {
            let current = path.to_string();
            if ctx.first_zero_report.is_none() {
                ctx.first_zero_report = Some(current.clone());
            }
            if let Some(non_zero) = ctx.first_non_zero_report.as_ref() {
                return Err(HidDescriptorSynthesisError::MixedReportIds {
                    zero_path: current,
                    non_zero_path: non_zero.clone(),
                });
            }
        }
    } else {
        if ctx.first_non_zero_report.is_none() || ctx.first_zero_report.is_some() {
            let current = path.to_string();
            if ctx.first_non_zero_report.is_none() {
                ctx.first_non_zero_report = Some(current.clone());
            }
            if let Some(zero) = ctx.first_zero_report.as_ref() {
                return Err(HidDescriptorSynthesisError::MixedReportIds {
                    zero_path: zero.clone(),
                    non_zero_path: current,
                });
            }
        }
    }
    Ok(())
}

fn convert_report(
    kind: HidReportKindPath,
    report: &HidReportInfo,
    path: &mut HidMetadataPath,
    ctx: &mut SynthesisValidationContext,
) -> Result<report_descriptor::HidReportInfo> {
    if report.report_id > 0xFF {
        return Err(HidDescriptorSynthesisError::ReportIdOutOfRange {
            path: path.to_string(),
            report_id: report.report_id,
        });
    }

    validate_mixed_report_ids(report.report_id, path, ctx)?;

    Ok(report_descriptor::HidReportInfo {
        report_id: report.report_id,
        items: report
            .items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                path.push_item(idx);
                let converted = convert_item(kind, item, path);
                path.pop();
                converted
            })
            .collect::<Result<_>>()?,
    })
}

fn convert_item(
    kind: HidReportKindPath,
    item: &HidReportItem,
    path: &HidMetadataPath,
) -> Result<report_descriptor::HidReportItem> {
    validate_u16_range(item.usage_page, "usagePage", path)?;

    if !(-8..=7).contains(&item.unit_exponent) {
        return Err(HidDescriptorSynthesisError::UnitExponentOutOfRange {
            path: path.to_string(),
            unit_exponent: item.unit_exponent,
        });
    }

    if item.report_size == 0 {
        return Err(HidDescriptorSynthesisError::ReportSizeZero {
            path: path.to_string(),
        });
    }

    if item.report_size.checked_mul(item.report_count).is_none() {
        return Err(HidDescriptorSynthesisError::ReportBitLengthOverflow {
            path: path.to_string(),
            report_size: item.report_size,
            report_count: item.report_count,
        });
    }

    let usages = if item.is_range {
        validate_u16_range(item.usage_minimum, "usageMinimum", path)?;
        validate_u16_range(item.usage_maximum, "usageMaximum", path)?;
        if item.usage_minimum > item.usage_maximum {
            return Err(HidDescriptorSynthesisError::InvalidUsageRange {
                path: path.to_string(),
                min: item.usage_minimum,
                max: item.usage_maximum,
            });
        }

        // Normalized WebHID metadata generally keeps the expanded usages list even for `isRange`
        // items. Ensure we never silently truncate non-contiguous lists to `[min,max]`.
        if !item.usages.is_empty() && item.usages.len() <= MAX_RANGE_CONTIGUITY_CHECK_LEN {
            for &usage in &item.usages {
                validate_u16_range(usage, "usages[]", path)?;
            }

            let mut sorted = item.usages.clone();
            sorted.sort_unstable();
            sorted.dedup();
            let min = *sorted.first().expect("non-empty usages");
            let max = *sorted.last().expect("non-empty usages");
            let contiguous = if min == max {
                true
            } else if sorted.len() == 2 {
                // Support legacy `[min, max]` representation without requiring the expanded list.
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

            if !contiguous || min != item.usage_minimum || max != item.usage_maximum {
                return Err(HidDescriptorSynthesisError::InvalidRangeUsages {
                    path: path.to_string(),
                });
            }
        }

        vec![item.usage_minimum, item.usage_maximum]
    } else {
        if item.usages.len() > MAX_EXPLICIT_USAGES {
            return Err(HidDescriptorSynthesisError::UsagesTooLong {
                path: path.to_string(),
                len: item.usages.len(),
                max: MAX_EXPLICIT_USAGES,
            });
        }
        for &usage in &item.usages {
            validate_u16_range(usage, "usages[]", path)?;
        }
        item.usages.clone()
    };

    // HID 1.11:
    // - Input main items do not have a Volatile flag (bit7 is Buffered Bytes for Input).
    // - Output/Feature main items use bit7 for Volatile.
    let is_volatile = match kind {
        HidReportKindPath::Input => false,
        HidReportKindPath::Output | HidReportKindPath::Feature => item.is_volatile,
    };

    Ok(report_descriptor::HidReportItem {
        is_array: item.is_array,
        is_absolute: item.is_absolute,
        is_buffered_bytes: item.is_buffered_bytes,
        is_volatile,
        is_constant: item.is_constant,
        is_wrapped: item.is_wrapped,
        is_linear: item.is_linear,
        has_preferred_state: item.has_preferred_state,
        has_null: item.has_null,
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
            collection_type: HidCollectionType::Application,
            children: vec![],
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![item],
            }],
            output_reports: vec![],
            feature_reports: vec![],
        }]
    }

    fn make_output_collections(item: HidReportItem) -> Vec<HidCollectionInfo> {
        vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: HidCollectionType::Application,
            children: vec![],
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![item],
            }],
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
            Err(HidDescriptorSynthesisError::UnitExponentOutOfRange {
                path,
                unit_exponent,
            }) => {
                assert_eq!(path, "collections[0]/inputReports[0](reportId=0)/items[0]");
                assert_eq!(unit_exponent, 8);
            }
            other => panic!("expected UnitExponentOutOfRange error, got {other:?}"),
        }
    }

    #[test]
    fn report_id_out_of_range_is_rejected_with_path() {
        let item = make_item(0);
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: HidCollectionType::Application,
            children: vec![HidCollectionInfo {
                usage_page: 0x01,
                usage: 0x02,
                collection_type: HidCollectionType::Physical,
                children: vec![],
                input_reports: vec![HidReportInfo {
                    report_id: 999,
                    items: vec![item],
                }],
                output_reports: vec![],
                feature_reports: vec![],
            }],
            input_reports: vec![],
            output_reports: vec![],
            feature_reports: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorSynthesisError::ReportIdOutOfRange { path, report_id }) => {
                assert_eq!(report_id, 999);
                assert_eq!(
                    path,
                    "collections[0]/children[0]/inputReports[0](reportId=999)"
                );
            }
            other => panic!("expected ReportIdOutOfRange error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_usage_range_is_rejected_with_item_path() {
        let mut item = make_item(0);
        item.is_range = true;
        item.usages = vec![];
        item.usage_minimum = 10;
        item.usage_maximum = 5;
        let collections = make_collections(item);

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorSynthesisError::InvalidUsageRange { path, min, max }) => {
                assert_eq!(path, "collections[0]/inputReports[0](reportId=0)/items[0]");
                assert_eq!(min, 10);
                assert_eq!(max, 5);
            }
            other => panic!("expected InvalidUsageRange error, got {other:?}"),
        }
    }

    #[test]
    fn mixed_report_ids_are_rejected_with_example_paths() {
        let item = make_item(0);
        let collections = vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: HidCollectionType::Application,
            children: vec![HidCollectionInfo {
                usage_page: 0x01,
                usage: 0x02,
                collection_type: HidCollectionType::Physical,
                children: vec![],
                input_reports: vec![HidReportInfo {
                    report_id: 1,
                    items: vec![item.clone()],
                }],
                output_reports: vec![],
                feature_reports: vec![],
            }],
            input_reports: vec![HidReportInfo {
                report_id: 0,
                items: vec![item],
            }],
            output_reports: vec![],
            feature_reports: vec![],
        }];

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorSynthesisError::MixedReportIds {
                zero_path,
                non_zero_path,
            }) => {
                assert_eq!(zero_path, "collections[0]/inputReports[0](reportId=0)");
                assert_eq!(
                    non_zero_path,
                    "collections[0]/children[0]/inputReports[0](reportId=1)"
                );
            }
            other => panic!("expected MixedReportIds error, got {other:?}"),
        }
    }

    #[test]
    fn report_size_zero_is_rejected_with_item_path() {
        let mut item = make_item(0);
        item.report_size = 0;
        let collections = make_collections(item);

        match synthesize_report_descriptor(&collections) {
            Err(HidDescriptorSynthesisError::ReportSizeZero { path }) => {
                assert_eq!(path, "collections[0]/inputReports[0](reportId=0)/items[0]");
            }
            other => panic!("expected ReportSizeZero error, got {other:?}"),
        }
    }

    #[test]
    fn buffered_bytes_uses_bit7_for_input_main_items() {
        let mut item = make_item(0);
        // Ensure the main-item flag byte is otherwise zero so we can assert the exact
        // spec-canonical encoding.
        item.is_array = true;
        item.is_buffered_bytes = true;
        // Input items do not have a Volatile bit; ensure it is ignored.
        item.is_volatile = true;
        let collections = make_collections(item);

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(2).any(|w| w == [0x81, 0x80]),
            "expected spec-canonical Input Buffered Bytes encoding (0x81 0x80): {desc:02x?}"
        );
        assert!(
            !desc.windows(3).any(|w| w == [0x82, 0x00, 0x01]),
            "did not expect Input Buffered Bytes to be encoded as a 2-byte payload (0x82 0x00 0x01): {desc:02x?}"
        );
    }

    #[test]
    fn hat_switch_null_state_synthesizes_to_input_0x42() {
        let mut item = make_item(0);
        // Spec-canonical hat switch main item flags: Data,Var,Abs,Null (0x42).
        item.has_null = true;
        let collections = make_collections(item);

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(2).any(|w| w == [0x81, 0x42]),
            "expected Input item with Null State flag (0x81 0x42): {desc:02x?}"
        );
    }

    #[test]
    fn buffered_bytes_uses_bit8_for_output_main_items() {
        let mut item = make_item(0);
        // Keep the low-byte flags clear so the expected 0x0100 encoding is stable.
        item.is_array = true;
        item.is_buffered_bytes = true;
        let collections = make_output_collections(item);

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(3).any(|w| w == [0x92, 0x00, 0x01]),
            "expected spec-canonical Output Buffered Bytes encoding (0x92 0x00 0x01): {desc:02x?}"
        );
    }

    #[test]
    fn volatile_sets_bit7_for_output_main_items() {
        let mut item = make_item(0);
        item.is_array = true;
        item.is_volatile = true;
        let collections = make_output_collections(item);

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(2).any(|w| w == [0x91, 0x80]),
            "expected spec-canonical Output Volatile encoding (0x91 0x80): {desc:02x?}"
        );
    }

    #[test]
    fn range_items_use_expanded_usages_list_for_min_max_emission() {
        let mut item = make_item(0);
        item.usage_page = 0x07;
        item.usages = (0xE0u32..=0xE7u32).collect();
        item.usage_minimum = 0xE0;
        item.usage_maximum = 0xE7;
        item.is_range = true;
        item.report_size = 1;
        item.report_count = 8;
        let collections = make_collections(item);

        let desc = synthesize_report_descriptor(&collections).unwrap();
        assert!(
            desc.windows(4).any(|w| w == [0x19, 0xE0, 0x29, 0xE7]),
            "expected Usage Minimum/Maximum (E0..E7) in descriptor: {desc:02x?}"
        );
    }
}
