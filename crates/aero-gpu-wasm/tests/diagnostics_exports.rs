#![cfg(target_arch = "wasm32")]

use aero_gpu_wasm::{drain_gpu_events, get_gpu_stats};
use js_sys::{Array, Reflect};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn get_gpu_stats_returns_object_with_expected_counters() {
    let stats = get_gpu_stats();
    assert!(stats.is_object());

    for key in [
        "presents_attempted",
        "presents_succeeded",
        "recoveries_attempted",
        "recoveries_succeeded",
        "surface_reconfigures",
    ] {
        let value = Reflect::get(&stats, &JsValue::from_str(key)).expect("Reflect::get");
        assert!(
            value.as_f64().is_some(),
            "expected {key} to be a JS number, got {value:?}"
        );
    }
}

#[wasm_bindgen_test]
fn drain_gpu_events_returns_array_and_is_non_panicking() {
    // No events are guaranteed in this test environment; validate shape and that
    // the export is safe to call repeatedly.
    let first = drain_gpu_events();
    assert!(Array::is_array(&first));

    let second = drain_gpu_events();
    assert!(Array::is_array(&second));
}

