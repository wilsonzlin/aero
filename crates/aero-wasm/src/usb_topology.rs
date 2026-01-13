#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

/// Parse a USB topology "path" from a JS value.
///
/// Path semantics match the UHCI bridges for consistency:
/// - `path[0]`: root port index (0-based)
/// - `path[1..]`: hub ports (1-based, i.e. 1..=255)
pub(crate) fn parse_usb_path(path: JsValue, max_root_port: u8) -> Result<Vec<u8>, JsValue> {
    let parts: Vec<u32> = serde_wasm_bindgen::from_value(path)
        .map_err(|e| js_error(format!("Invalid USB topology path: {e}")))?;
    if parts.is_empty() {
        return Err(js_error("USB topology path must not be empty"));
    }

    let mut out = Vec::with_capacity(parts.len());
    for (i, part) in parts.into_iter().enumerate() {
        if i == 0 {
            if part > u32::from(max_root_port) {
                return Err(js_error(format!(
                    "USB root port must be in 0..={max_root_port}"
                )));
            }
            out.push(part as u8);
            continue;
        }

        if !(1..=255).contains(&part) {
            return Err(js_error("USB hub port numbers must be in 1..=255"));
        }
        out.push(part as u8);
    }

    Ok(out)
}
