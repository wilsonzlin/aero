#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use js_sys::Reflect;

use aero_jit_x86::jit_ctx::{
    CODE_VERSION_TABLE_LEN_OFFSET, CODE_VERSION_TABLE_PTR_OFFSET, TIER2_CTX_OFFSET, TIER2_CTX_SIZE,
    TRACE_EXIT_REASON_OFFSET,
};

#[track_caller]
fn read_u32(obj: &JsValue, key: &str) -> u32 {
    Reflect::get(obj, &JsValue::from_str(key))
        .unwrap_or_else(|_| panic!("missing jit_abi_constants key: {key}"))
        .as_f64()
        .unwrap_or_else(|| panic!("jit_abi_constants[{key}] must be a number"))
        .round() as u32
}

#[wasm_bindgen_test]
fn jit_abi_constants_export_code_version_table_offsets() {
    let obj = aero_wasm::jit_abi_constants();
    assert!(obj.is_object(), "jit_abi_constants must return an object");

    assert_eq!(
        read_u32(&obj, "trace_exit_reason_offset"),
        TRACE_EXIT_REASON_OFFSET
    );
    assert_eq!(
        read_u32(&obj, "code_version_table_ptr_offset"),
        CODE_VERSION_TABLE_PTR_OFFSET
    );
    assert_eq!(
        read_u32(&obj, "code_version_table_len_offset"),
        CODE_VERSION_TABLE_LEN_OFFSET
    );

    // Keep the same layout contract that JS relies on: Tier-2 ctx layout is stable and
    // the code-version table fields follow the exit reason.
    let tier2_ctx_offset = read_u32(&obj, "tier2_ctx_offset");
    assert_eq!(tier2_ctx_offset, TIER2_CTX_OFFSET);
    assert_eq!(read_u32(&obj, "tier2_ctx_size"), TIER2_CTX_SIZE);
    assert_eq!(
        read_u32(&obj, "code_version_table_ptr_offset"),
        tier2_ctx_offset + 4
    );
    assert_eq!(
        read_u32(&obj, "code_version_table_len_offset"),
        tier2_ctx_offset + 8
    );
}
