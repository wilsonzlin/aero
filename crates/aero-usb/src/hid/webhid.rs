use serde::de::{Error as DeError, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use alloc::collections::BTreeMap;

use super::report_descriptor;
fn default_true() -> bool {
    true
}

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

            fn visit_f64<E>(self, value: f64) -> core::result::Result<Self::Value, E>
            where
                E: DeError,
            {
                if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > 0xFF as f64
                {
                    return Err(E::invalid_value(Unexpected::Float(value), &self));
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
    pub is_absolute: bool,
    pub is_array: bool,
    pub is_buffered_bytes: bool,
    pub is_constant: bool,
    #[serde(default = "default_true")]
    pub is_linear: bool,
    pub is_range: bool,
    #[serde(default)]
    pub is_relative: bool,
    #[serde(default)]
    pub is_volatile: bool,
    #[serde(default)]
    pub has_null: bool,
    #[serde(default = "default_true")]
    pub has_preferred_state: bool,
    #[serde(default, alias = "wrap")]
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
/// used by [`crate::hid::report_descriptor`] and then reuses the canonical
/// short-item encoder.
pub fn synthesize_report_descriptor(collections: &[HidCollectionInfo]) -> Result<Vec<u8>> {
    let converted: Vec<report_descriptor::HidCollectionInfo> = collections
        .iter()
        .map(convert_collection)
        .collect::<Result<_>>()?;
    Ok(report_descriptor::synthesize_report_descriptor(&converted)?)
}

/// Infer a HID boot interface subclass/protocol from normalized WebHID collection metadata.
///
/// Some legacy OS/BIOS-era USB stacks rely on `bInterfaceSubClass`/`bInterfaceProtocol` to identify
/// keyboards/mice (USB HID boot protocol). WebHID devices often omit this hint, but the WebHID
/// collection tree usually contains enough information to make a conservative inference.
///
/// Inference rules:
/// - Scan only the *top-level* collections.
/// - Consider only `Application` collections on the Generic Desktop page (`usagePage == 0x01`).
/// - If exactly one of:
///   - `usage == 0x06` (Keyboard), or
///   - `usage == 0x02` (Mouse)
///     is present, return:
///   - subclass = 0x01 (Boot), and
///   - protocol = 0x01 (Keyboard) or 0x02 (Mouse).
/// - If both are present or neither is present, return `None` (do not guess).
pub fn infer_boot_interface(collections: &[HidCollectionInfo]) -> Option<(u8, u8)> {
    let mut has_keyboard = false;
    let mut has_mouse = false;

    for col in collections {
        if col.collection_type != HidCollectionType::Application {
            continue;
        }
        if col.usage_page != 0x01 {
            continue;
        }
        match col.usage {
            0x06 => has_keyboard = true,
            0x02 => has_mouse = true,
            _ => {}
        }
    }

    match (has_keyboard, has_mouse) {
        (true, false) => Some((0x01, 0x01)),
        (false, true) => Some((0x01, 0x02)),
        _ => None,
    }
}

/// Backwards-compatible alias (historical name used by some callers).
pub fn infer_boot_interface_subclass_protocol(
    collections: &[HidCollectionInfo],
) -> Option<(u8, u8)> {
    infer_boot_interface(collections)
}

/// Compute the maximum output report size, in bytes, as it would appear on the USB wire.
///
/// This mirrors the browser/TypeScript WebHID policy (`computeMaxOutputReportBytesOnWire`):
/// - Aggregate output report bits across the entire collection tree *by report ID*.
/// - For each report ID:
///   - `payloadBytes = ceil(totalBits / 8)`
///   - `onWireBytes = payloadBytes + (reportId != 0 ? 1 : 0)` (report ID prefix byte)
/// - Return the maximum `onWireBytes` across all output report IDs (or 0 when none exist).
///
/// Note: This uses saturating arithmetic to avoid panics on malformed/hostile inputs.
pub fn max_output_report_bytes_on_wire(collections: &[HidCollectionInfo]) -> u32 {
    fn report_bits(report: &HidReportInfo) -> u64 {
        let mut bits: u64 = 0;
        for item in &report.items {
            let size = u64::from(item.report_size);
            let count = u64::from(item.report_count);
            bits = bits.saturating_add(size.saturating_mul(count));
        }
        bits
    }

    fn walk_collection(col: &HidCollectionInfo, bits_by_id: &mut BTreeMap<u32, u64>) {
        for report in &col.output_reports {
            let bits = report_bits(report);
            bits_by_id
                .entry(report.report_id)
                .and_modify(|v| *v = v.saturating_add(bits))
                .or_insert(bits);
        }
        for child in &col.children {
            walk_collection(child, bits_by_id);
        }
    }

    let mut bits_by_id: BTreeMap<u32, u64> = BTreeMap::new();
    for col in collections {
        walk_collection(col, &mut bits_by_id);
    }

    let mut max_on_wire: u64 = 0;
    for (&report_id, &bits) in &bits_by_id {
        let payload_bytes = bits.saturating_add(7) / 8;
        let on_wire_bytes = if report_id == 0 {
            payload_bytes
        } else {
            payload_bytes.saturating_add(1)
        };
        max_on_wire = max_on_wire.max(on_wire_bytes);
    }

    u32::try_from(max_on_wire).unwrap_or(u32::MAX)
}

/// Backwards-compatible alias (historical name used by some callers).
pub fn max_output_report_bytes(collections: &[HidCollectionInfo]) -> u32 {
    max_output_report_bytes_on_wire(collections)
}

#[derive(Debug, Clone, Copy)]
enum HidReportKindPath {
    Input,
    Output,
    Feature,
}

fn convert_collection(
    collection: &HidCollectionInfo,
) -> Result<report_descriptor::HidCollectionInfo> {
    Ok(report_descriptor::HidCollectionInfo {
        usage_page: collection.usage_page,
        usage: collection.usage,
        collection_type: collection.collection_type.code(),
        input_reports: collection
            .input_reports
            .iter()
            .map(|report| convert_report(HidReportKindPath::Input, report))
            .collect::<Result<_>>()?,
        output_reports: collection
            .output_reports
            .iter()
            .map(|report| convert_report(HidReportKindPath::Output, report))
            .collect::<Result<_>>()?,
        feature_reports: collection
            .feature_reports
            .iter()
            .map(|report| convert_report(HidReportKindPath::Feature, report))
            .collect::<Result<_>>()?,
        children: collection
            .children
            .iter()
            .map(convert_collection)
            .collect::<Result<_>>()?,
    })
}

fn convert_report(
    kind: HidReportKindPath,
    report: &HidReportInfo,
) -> Result<report_descriptor::HidReportInfo> {
    Ok(report_descriptor::HidReportInfo {
        report_id: report.report_id,
        items: report
            .items
            .iter()
            .map(|item| convert_item(kind, item))
            .collect::<Result<_>>()?,
    })
}

fn convert_item(
    kind: HidReportKindPath,
    item: &HidReportItem,
) -> Result<report_descriptor::HidReportItem> {
    let usages = if item.is_range {
        vec![item.usage_minimum, item.usage_maximum]
    } else {
        item.usages.clone()
    };

    let (string_minimum, string_maximum) = if item.string_minimum == 0 && item.string_maximum == 0 {
        (None, None)
    } else {
        (Some(item.string_minimum), Some(item.string_maximum))
    };
    let (designator_minimum, designator_maximum) =
        if item.designator_minimum == 0 && item.designator_maximum == 0 {
            (None, None)
        } else {
            (Some(item.designator_minimum), Some(item.designator_maximum))
        };

    Ok(report_descriptor::HidReportItem {
        is_array: item.is_array,
        is_absolute: item.is_absolute,
        is_buffered_bytes: item.is_buffered_bytes,
        is_volatile: match kind {
            HidReportKindPath::Input => false,
            HidReportKindPath::Output | HidReportKindPath::Feature => item.is_volatile,
        },
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
        strings: item.strings.clone(),
        string_minimum,
        string_maximum,
        designators: item.designators.clone(),
        designator_minimum,
        designator_maximum,
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

    fn make_output_collections(
        report_id: u32,
        report_size: u32,
        report_count: u32,
    ) -> Vec<HidCollectionInfo> {
        vec![HidCollectionInfo {
            usage_page: 0x01,
            usage: 0x02,
            collection_type: HidCollectionType::Application,
            children: vec![],
            input_reports: vec![],
            output_reports: vec![HidReportInfo {
                report_id,
                items: vec![HidReportItem {
                    usage_page: 0x01,
                    usages: vec![0x30],
                    usage_minimum: 0,
                    usage_maximum: 0,
                    report_size,
                    report_count,
                    unit_exponent: 0,
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
                }],
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
            Err(HidDescriptorSynthesisError::HidDescriptor(err)) => match err {
                report_descriptor::HidDescriptorError::Validation { path, message } => {
                    assert_eq!(path, "collections[0].inputReports[0].items[0]");
                    assert!(message.contains("unitExponent"));
                }
                other => panic!("expected validation error, got {other:?}"),
            },
            other => panic!("expected HidDescriptor error, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_accepts_wrap_alias() {
        let collection: HidCollectionInfo = serde_json::from_str(
            r#"{
              "usagePage": 1,
              "usage": 0,
              "collectionType": 1,
              "children": [],
              "inputReports": [
                {
                  "reportId": 0,
                  "items": [
                    {
                      "usagePage": 1,
                      "usages": [48],
                      "usageMinimum": 0,
                      "usageMaximum": 0,
                      "reportSize": 8,
                      "reportCount": 1,
                      "unitExponent": 0,
                      "unit": 0,
                      "logicalMinimum": 0,
                      "logicalMaximum": 1,
                      "physicalMinimum": 0,
                      "physicalMaximum": 0,
                      "strings": [],
                      "stringMinimum": 0,
                      "stringMaximum": 0,
                      "designators": [],
                      "designatorMinimum": 0,
                      "designatorMaximum": 0,
                      "isAbsolute": true,
                      "isArray": true,
                      "isBufferedBytes": false,
                      "isConstant": false,
                      "isLinear": true,
                      "isRange": false,
                      "isRelative": false,
                      "isVolatile": false,
                      "hasNull": false,
                      "hasPreferredState": true,
                      "wrap": true
                    }
                  ]
                }
              ],
              "outputReports": [],
              "featureReports": []
            }"#,
        )
        .expect("deserialize wrap alias form");

        assert!(collection.input_reports[0].items[0].is_wrapped);
    }

    #[test]
    fn deserialize_allows_missing_is_relative_field() {
        // Some WebHID typings omit `isRelative` (it can be derived from `isAbsolute`).
        let collection: HidCollectionInfo = serde_json::from_str(
            r#"{
              "usagePage": 1,
              "usage": 0,
              "collectionType": 1,
              "children": [],
              "inputReports": [
                {
                  "reportId": 0,
                  "items": [
                    {
                      "usagePage": 1,
                      "usages": [48],
                      "usageMinimum": 0,
                      "usageMaximum": 0,
                      "reportSize": 8,
                      "reportCount": 1,
                      "unitExponent": 0,
                      "unit": 0,
                      "logicalMinimum": 0,
                      "logicalMaximum": 1,
                      "physicalMinimum": 0,
                      "physicalMaximum": 0,
                      "strings": [],
                      "stringMinimum": 0,
                      "stringMaximum": 0,
                      "designators": [],
                      "designatorMinimum": 0,
                      "designatorMaximum": 0,
                      "isAbsolute": false,
                      "isArray": true,
                      "isBufferedBytes": false,
                      "isConstant": false,
                      "isLinear": true,
                      "isRange": false,
                      "isVolatile": false,
                      "hasNull": false,
                      "hasPreferredState": true,
                      "isWrapped": false
                    }
                  ]
                }
              ],
              "outputReports": [],
              "featureReports": []
            }"#,
        )
        .expect("deserialize without isRelative field");

        assert!(!collection.input_reports[0].items[0].is_absolute);
    }

    #[test]
    fn deserialize_accepts_collection_type_as_type_string_alias() {
        let collection: HidCollectionInfo = serde_json::from_str(
            r#"{
              "usagePage": 1,
              "usage": 0,
              "type": "application",
              "children": [],
              "inputReports": [],
              "outputReports": [],
              "featureReports": []
            }"#,
        )
        .expect("deserialize type string alias form");

        assert_eq!(collection.collection_type, HidCollectionType::Application);
    }

    #[test]
    fn deserialize_accepts_collection_type_as_type_numeric_alias() {
        let collection: HidCollectionInfo = serde_json::from_str(
            r#"{
              "usagePage": 1,
              "usage": 0,
              "type": 1,
              "children": [],
              "inputReports": [],
              "outputReports": [],
              "featureReports": []
            }"#,
        )
        .expect("deserialize type numeric alias form");

        assert_eq!(collection.collection_type, HidCollectionType::Application);
    }

    #[test]
    fn deserialize_accepts_collection_type_as_collectiontype_float_code() {
        let collection: HidCollectionInfo = serde_json::from_str(
            r#"{
              "usagePage": 1,
              "usage": 0,
              "collectionType": 1.0,
              "children": [],
              "inputReports": [],
              "outputReports": [],
              "featureReports": []
            }"#,
        )
        .expect("deserialize collectionType float form");

        assert_eq!(collection.collection_type, HidCollectionType::Application);
    }

    #[test]
    fn collection_type_serializes_as_numeric_collection_type_field() {
        let collection: HidCollectionInfo = serde_json::from_str(
            r#"{
              "usagePage": 1,
              "usage": 0,
              "type": "application",
              "children": [],
              "inputReports": [],
              "outputReports": [],
              "featureReports": []
            }"#,
        )
        .expect("deserialize type string form");

        let value = serde_json::to_value(&collection).expect("serialize collection");

        assert_eq!(value.get("collectionType"), Some(&serde_json::json!(1)));
        assert!(
            value.get("type").is_none(),
            "normalized JSON should not use the WebHID string enum field name: {value}"
        );
    }

    #[test]
    fn max_output_report_bytes_on_wire_is_zero_when_no_output_reports_exist() {
        let collections = make_collections(make_item(0));
        assert_eq!(max_output_report_bytes_on_wire(&collections), 0);
    }

    #[test]
    fn max_output_report_bytes_on_wire_counts_report_id_prefix_byte() {
        // 2 bytes payload, no report-id prefix.
        let collections = make_output_collections(0, 8, 2);
        assert_eq!(max_output_report_bytes_on_wire(&collections), 2);

        // 2 bytes payload, plus report-id prefix.
        let collections = make_output_collections(1, 8, 2);
        assert_eq!(max_output_report_bytes_on_wire(&collections), 3);
    }

    #[test]
    fn max_output_report_bytes_on_wire_handles_large_output_reports() {
        let collections = make_output_collections(0, 8, 65);
        assert_eq!(max_output_report_bytes_on_wire(&collections), 65);
    }
}
