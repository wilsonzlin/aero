#![cfg(target_arch = "wasm32")]

use aero_wasm::{add, demo_render_rgba8888};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn module_loads_and_exports_work() {
    assert_eq!(add(40, 2), 42);

    let mut buf = vec![0u8; 8 * 8 * 4];
    let offset = buf.as_mut_ptr() as u32;
    let written = demo_render_rgba8888(offset, 8, 8, 8 * 4, 1000.0);
    assert_eq!(written, 64);
    assert_eq!(&buf[0..4], &[60, 35, 20, 255]);
}
