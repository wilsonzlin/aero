#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use js_sys::Reflect;

#[wasm_bindgen_test]
fn storage_capabilities_exports_boolean_fields() {
    let caps = aero_wasm::storage_capabilities();
    assert!(
        caps.is_object(),
        "storage_capabilities must return an object"
    );

    for key in [
        "opfsSupported",
        "opfsSyncAccessSupported",
        "isWorkerScope",
        "crossOriginIsolated",
        "sharedArrayBufferSupported",
        "isSecureContext",
    ] {
        let v = Reflect::get(&caps, &JsValue::from_str(key))
            .unwrap_or_else(|_| panic!("missing storage_capabilities field: {key}"));
        assert!(
            v.as_bool().is_some(),
            "storage_capabilities[{key}] must be a boolean"
        );
    }
}
