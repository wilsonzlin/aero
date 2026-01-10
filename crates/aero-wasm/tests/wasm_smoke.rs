#![cfg(target_arch = "wasm32")]

use aero_wasm::add;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn module_loads_and_exports_work() {
    assert_eq!(add(40, 2), 42);
}
