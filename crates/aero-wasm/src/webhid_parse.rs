#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use js_sys::{Array, Reflect};

use aero_usb::hid::webhid;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn expect_object(value: &JsValue, ctx: &str) -> Result<(), JsValue> {
    if value.is_null() || value.is_undefined() {
        return Err(js_error(&format!("{ctx}: expected an object")));
    }
    if !value.is_object() || Array::is_array(value) {
        return Err(js_error(&format!("{ctx}: expected an object")));
    }
    Ok(())
}

fn expect_array(value: &JsValue, ctx: &str) -> Result<Array, JsValue> {
    if !Array::is_array(value) {
        return Err(js_error(&format!("{ctx}: expected an array")));
    }
    Ok(value.clone().unchecked_into::<Array>())
}

fn get_prop(obj: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    Reflect::get(obj, &JsValue::from_str(key))
        .map_err(|err| js_error(&format!("Failed to read property '{key}': {err:?}")))
}

fn parse_u32(value: &JsValue, ctx: &str) -> Result<u32, JsValue> {
    let Some(v) = value.as_f64() else {
        return Err(js_error(&format!("{ctx}: expected a number")));
    };
    if !v.is_finite() || v.fract() != 0.0 || v < 0.0 || v > u32::MAX as f64 {
        return Err(js_error(&format!("{ctx}: expected a u32 number")));
    }
    Ok(v as u32)
}

fn parse_i32(value: &JsValue, ctx: &str) -> Result<i32, JsValue> {
    let Some(v) = value.as_f64() else {
        return Err(js_error(&format!("{ctx}: expected a number")));
    };
    if !v.is_finite() || v.fract() != 0.0 || v < i32::MIN as f64 || v > i32::MAX as f64 {
        return Err(js_error(&format!("{ctx}: expected an i32 number")));
    }
    Ok(v as i32)
}

fn parse_bool(value: &JsValue, ctx: &str) -> Result<bool, JsValue> {
    value
        .as_bool()
        .ok_or_else(|| js_error(&format!("{ctx}: expected a boolean")))
}

fn parse_bool_with_default(value: &JsValue, default: bool, ctx: &str) -> Result<bool, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(default);
    }
    parse_bool(value, ctx)
}

fn parse_u32_array(value: &JsValue, ctx: &str) -> Result<Vec<u32>, JsValue> {
    let arr = expect_array(value, ctx)?;
    let len = arr.length();
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let el = arr.get(i);
        out.push(parse_u32(&el, ctx)?);
    }
    Ok(out)
}

fn parse_collection_type(value: &JsValue) -> Result<webhid::HidCollectionType, JsValue> {
    if let Some(v) = value.as_f64() {
        if !v.is_finite() || v.fract() != 0.0 || v < 0.0 || v > 0xff as f64 {
            return Err(js_error(
                "collectionType: expected a numeric HID collection type code (0..=6)",
            ));
        }
        let code = v as u8;
        return match code {
            0x00 => Ok(webhid::HidCollectionType::Physical),
            0x01 => Ok(webhid::HidCollectionType::Application),
            0x02 => Ok(webhid::HidCollectionType::Logical),
            0x03 => Ok(webhid::HidCollectionType::Report),
            0x04 => Ok(webhid::HidCollectionType::NamedArray),
            0x05 => Ok(webhid::HidCollectionType::UsageSwitch),
            0x06 => Ok(webhid::HidCollectionType::UsageModifier),
            _ => Err(js_error(
                "collectionType: expected a numeric HID collection type code (0..=6)",
            )),
        };
    }
    if let Some(s) = value.as_string() {
        return match s.as_str() {
            "physical" => Ok(webhid::HidCollectionType::Physical),
            "application" => Ok(webhid::HidCollectionType::Application),
            "logical" => Ok(webhid::HidCollectionType::Logical),
            "report" => Ok(webhid::HidCollectionType::Report),
            "namedArray" => Ok(webhid::HidCollectionType::NamedArray),
            "usageSwitch" => Ok(webhid::HidCollectionType::UsageSwitch),
            "usageModifier" => Ok(webhid::HidCollectionType::UsageModifier),
            _ => Err(js_error(
                "collectionType: expected a HID collection type string enum or numeric code",
            )),
        };
    }
    Err(js_error(
        "collectionType: expected a HID collection type string enum or numeric code",
    ))
}

fn parse_report_item(value: &JsValue) -> Result<webhid::HidReportItem, JsValue> {
    expect_object(value, "HID report item")?;

    let usage_page = parse_u32(&get_prop(value, "usagePage")?, "usagePage")?;
    let usages = parse_u32_array(&get_prop(value, "usages")?, "usages")?;
    let usage_minimum = parse_u32(&get_prop(value, "usageMinimum")?, "usageMinimum")?;
    let usage_maximum = parse_u32(&get_prop(value, "usageMaximum")?, "usageMaximum")?;
    let report_size = parse_u32(&get_prop(value, "reportSize")?, "reportSize")?;
    let report_count = parse_u32(&get_prop(value, "reportCount")?, "reportCount")?;
    let unit_exponent = parse_i32(&get_prop(value, "unitExponent")?, "unitExponent")?;
    let unit = parse_u32(&get_prop(value, "unit")?, "unit")?;
    let logical_minimum = parse_i32(&get_prop(value, "logicalMinimum")?, "logicalMinimum")?;
    let logical_maximum = parse_i32(&get_prop(value, "logicalMaximum")?, "logicalMaximum")?;
    let physical_minimum = parse_i32(&get_prop(value, "physicalMinimum")?, "physicalMinimum")?;
    let physical_maximum = parse_i32(&get_prop(value, "physicalMaximum")?, "physicalMaximum")?;
    let strings = parse_u32_array(&get_prop(value, "strings")?, "strings")?;
    let string_minimum = parse_u32(&get_prop(value, "stringMinimum")?, "stringMinimum")?;
    let string_maximum = parse_u32(&get_prop(value, "stringMaximum")?, "stringMaximum")?;
    let designators = parse_u32_array(&get_prop(value, "designators")?, "designators")?;
    let designator_minimum =
        parse_u32(&get_prop(value, "designatorMinimum")?, "designatorMinimum")?;
    let designator_maximum =
        parse_u32(&get_prop(value, "designatorMaximum")?, "designatorMaximum")?;
    let is_absolute = parse_bool(&get_prop(value, "isAbsolute")?, "isAbsolute")?;
    let is_array = parse_bool(&get_prop(value, "isArray")?, "isArray")?;
    let is_buffered_bytes = parse_bool(&get_prop(value, "isBufferedBytes")?, "isBufferedBytes")?;
    let is_constant = parse_bool(&get_prop(value, "isConstant")?, "isConstant")?;
    let is_linear = parse_bool_with_default(&get_prop(value, "isLinear")?, true, "isLinear")?;
    let is_range = parse_bool(&get_prop(value, "isRange")?, "isRange")?;
    let is_relative =
        parse_bool_with_default(&get_prop(value, "isRelative")?, false, "isRelative")?;
    let is_volatile =
        parse_bool_with_default(&get_prop(value, "isVolatile")?, false, "isVolatile")?;
    let has_null = parse_bool_with_default(&get_prop(value, "hasNull")?, false, "hasNull")?;
    let has_preferred_state = parse_bool_with_default(
        &get_prop(value, "hasPreferredState")?,
        true,
        "hasPreferredState",
    )?;
    let is_wrapped = {
        let primary = get_prop(value, "isWrapped")?;
        if !primary.is_null() && !primary.is_undefined() {
            parse_bool(&primary, "isWrapped")?
        } else {
            let alias = get_prop(value, "wrap")?;
            parse_bool_with_default(&alias, false, "wrap")?
        }
    };

    Ok(webhid::HidReportItem {
        usage_page,
        usages,
        usage_minimum,
        usage_maximum,
        report_size,
        report_count,
        unit_exponent,
        unit,
        logical_minimum,
        logical_maximum,
        physical_minimum,
        physical_maximum,
        strings,
        string_minimum,
        string_maximum,
        designators,
        designator_minimum,
        designator_maximum,
        is_absolute,
        is_array,
        is_buffered_bytes,
        is_constant,
        is_linear,
        is_range,
        is_relative,
        is_volatile,
        has_null,
        has_preferred_state,
        is_wrapped,
    })
}

fn parse_report_info(value: &JsValue) -> Result<webhid::HidReportInfo, JsValue> {
    expect_object(value, "HID report info")?;
    let report_id = parse_u32(&get_prop(value, "reportId")?, "reportId")?;
    let items_val = get_prop(value, "items")?;
    let items_arr = expect_array(&items_val, "items")?;
    let len = items_arr.length();
    let mut items = Vec::with_capacity(len as usize);
    for i in 0..len {
        let item = items_arr.get(i);
        items.push(parse_report_item(&item)?);
    }
    Ok(webhid::HidReportInfo { report_id, items })
}

fn parse_reports(value: &JsValue, ctx: &str) -> Result<Vec<webhid::HidReportInfo>, JsValue> {
    let arr = expect_array(value, ctx)?;
    let len = arr.length();
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let report = arr.get(i);
        out.push(parse_report_info(&report)?);
    }
    Ok(out)
}

fn parse_collection(value: &JsValue) -> Result<webhid::HidCollectionInfo, JsValue> {
    expect_object(value, "HID collection")?;
    let usage_page = parse_u32(&get_prop(value, "usagePage")?, "usagePage")?;
    let usage = parse_u32(&get_prop(value, "usage")?, "usage")?;

    let collection_type_val = {
        let direct = get_prop(value, "collectionType")?;
        if !direct.is_null() && !direct.is_undefined() {
            direct
        } else {
            get_prop(value, "type")?
        }
    };
    let collection_type = parse_collection_type(&collection_type_val)?;

    let children_val = get_prop(value, "children")?;
    let children_arr = expect_array(&children_val, "children")?;
    let children_len = children_arr.length();
    let mut children = Vec::with_capacity(children_len as usize);
    for i in 0..children_len {
        let child = children_arr.get(i);
        children.push(parse_collection(&child)?);
    }

    let input_reports = parse_reports(&get_prop(value, "inputReports")?, "inputReports")?;
    let output_reports = parse_reports(&get_prop(value, "outputReports")?, "outputReports")?;
    let feature_reports = parse_reports(&get_prop(value, "featureReports")?, "featureReports")?;

    Ok(webhid::HidCollectionInfo {
        usage_page,
        usage,
        collection_type,
        children,
        input_reports,
        output_reports,
        feature_reports,
    })
}

/// Parse WebHID-normalized collection metadata from JS.
///
/// `collections_json` must be the normalized output of `normalizeCollections()` from
/// `web/src/hid/webhid_normalize.ts` (array of objects with camelCase fields).
pub fn parse_webhid_collections(
    collections_json: &JsValue,
) -> Result<Vec<webhid::HidCollectionInfo>, JsValue> {
    let arr = expect_array(collections_json, "collections")?;
    let len = arr.length();
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let col = arr.get(i);
        out.push(parse_collection(&col)?);
    }
    Ok(out)
}
