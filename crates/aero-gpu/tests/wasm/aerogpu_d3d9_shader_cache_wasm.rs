#![cfg(target_arch = "wasm32")]

use crate::common;
use aero_gpu::AerogpuD3d9Executor;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use js_sys::{Map, Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

struct PersistentCacheApiGuard {
    prev_api: Option<JsValue>,
    prev_store: Option<JsValue>,
}

impl PersistentCacheApiGuard {
    fn install(api: &JsValue, store: &JsValue) -> Self {
        let global = js_sys::global();
        let api_key = JsValue::from_str("AeroPersistentGpuCache");
        let store_key = JsValue::from_str("__aeroTestPersistentCache");

        let prev_api = Reflect::get(&global, &api_key)
            .ok()
            .and_then(|v| (!v.is_undefined()).then_some(v));
        let prev_store = Reflect::get(&global, &store_key)
            .ok()
            .and_then(|v| (!v.is_undefined()).then_some(v));

        Reflect::set(&global, &api_key, api).unwrap();
        Reflect::set(&global, &store_key, store).unwrap();

        Self { prev_api, prev_store }
    }
}

impl Drop for PersistentCacheApiGuard {
    fn drop(&mut self) {
        let global = js_sys::global();
        let api_key = JsValue::from_str("AeroPersistentGpuCache");
        let store_key = JsValue::from_str("__aeroTestPersistentCache");

        match &self.prev_api {
            Some(prev) => {
                let _ = Reflect::set(&global, &api_key, prev);
            }
            None => {
                let _ = Reflect::delete_property(&global, &api_key);
            }
        }

        match &self.prev_store {
            Some(prev) => {
                let _ = Reflect::set(&global, &store_key, prev);
            }
            None => {
                let _ = Reflect::delete_property(&global, &store_key);
            }
        }
    }
}

fn inc_counter(obj: &JsValue, field: &str) {
    let v = Reflect::get(obj, &JsValue::from_str(field))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let _ = Reflect::set(obj, &JsValue::from_str(field), &JsValue::from_f64(v + 1.0));
}

fn make_persistent_cache_stub() -> (JsValue, JsValue) {
    let store = Object::new();
    let map = Map::new();
    Reflect::set(&store, &JsValue::from_str("map"), &map).unwrap();
    Reflect::set(&store, &JsValue::from_str("getCalls"), &JsValue::from_f64(0.0)).unwrap();
    Reflect::set(&store, &JsValue::from_str("putCalls"), &JsValue::from_f64(0.0)).unwrap();
    Reflect::set(
        &store,
        &JsValue::from_str("deleteCalls"),
        &JsValue::from_f64(0.0),
    )
    .unwrap();
    Reflect::set(
        &store,
        &JsValue::from_str("openCalls"),
        &JsValue::from_f64(0.0),
    )
    .unwrap();
    let api = Object::new();

    // computeShaderCacheKey(dxbc, flags) -> string
    // Keep it deterministic and safe for use as a key.
    let compute_fn = Closure::<dyn FnMut(js_sys::Uint8Array, JsValue) -> JsValue>::wrap(Box::new(
        move |dxbc: js_sys::Uint8Array, flags: JsValue| -> JsValue {
            // Encode bytes as hex to keep key stable and easy to debug.
            let mut bytes = vec![0u8; dxbc.length() as usize];
            dxbc.copy_to(&mut bytes);
            let mut hex = String::new();
            for b in bytes {
                hex.push_str(&format!("{:02x}", b));
            }
            let flags_s = js_sys::JSON::stringify(&flags)
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "null".to_string());
            JsValue::from_str(&format!("test-key-{hex}-{flags_s}"))
        },
    ));
    Reflect::set(
        &api,
        &JsValue::from_str("computeShaderCacheKey"),
        compute_fn.as_ref().unchecked_ref(),
    )
    .unwrap();
    compute_fn.forget();

    // PersistentGpuCache.open() -> { getShader, putShader, deleteShader }
    let persistent_gpu_cache = Object::new();
    let open_store = store.clone().into();
    let open_map = map.clone();
    let open_fn = Closure::<dyn FnMut() -> JsValue>::wrap(Box::new(move || -> JsValue {
        inc_counter(&open_store, "openCalls");

        let inner = Object::new();

        // getShader(key) -> value | undefined
        let get_store = open_store.clone();
        let get_map = open_map.clone();
        let get_fn = Closure::<dyn FnMut(String) -> JsValue>::wrap(Box::new(move |key: String| {
            inc_counter(&get_store, "getCalls");
            get_map.get(&JsValue::from_str(&key))
        }));
        Reflect::set(&inner, &JsValue::from_str("getShader"), get_fn.as_ref().unchecked_ref())
            .unwrap();
        get_fn.forget();

        // putShader(key, value) -> void
        let put_store = open_store.clone();
        let put_map = open_map.clone();
        let put_fn =
            Closure::<dyn FnMut(String, JsValue) -> JsValue>::wrap(Box::new(move |key, value| {
                inc_counter(&put_store, "putCalls");
                put_map.set(&JsValue::from_str(&key), &value);
                JsValue::undefined()
            }));
        Reflect::set(&inner, &JsValue::from_str("putShader"), put_fn.as_ref().unchecked_ref())
            .unwrap();
        put_fn.forget();

        // deleteShader(key) -> void
        let del_store = open_store.clone();
        let del_map = open_map.clone();
        let del_fn = Closure::<dyn FnMut(String) -> JsValue>::wrap(Box::new(move |key| {
            inc_counter(&del_store, "deleteCalls");
            del_map.delete(&JsValue::from_str(&key));
            JsValue::undefined()
        }));
        Reflect::set(
            &inner,
            &JsValue::from_str("deleteShader"),
            del_fn.as_ref().unchecked_ref(),
        )
        .unwrap();
        del_fn.forget();

        inner.into()
    }));
    Reflect::set(
        &persistent_gpu_cache,
        &JsValue::from_str("open"),
        open_fn.as_ref().unchecked_ref(),
    )
    .unwrap();
    open_fn.forget();
    Reflect::set(
        &api,
        &JsValue::from_str("PersistentGpuCache"),
        &persistent_gpu_cache,
    )
    .unwrap();

    (api.into(), store.into())
}

fn read_f64(obj: &JsValue, field: &str) -> f64 {
    Reflect::get(obj, &JsValue::from_str(field))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    // The minimal translator only consumes opcode + "length" in bits 24..27.
    let token = (opcode as u32) | ((params.len() as u32) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn assemble_vs_pos_only() -> Vec<u8> {
    // vs_2_0
    let mut out = vec![0xFFFE0200];
    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // end
    out.push(0x0000FFFF);
    to_bytes(&out)
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_uses_persistent_shader_cache_on_wasm() {
    // Install a minimal stub `AeroPersistentGpuCache` so the Rust wasm persistent cache wrapper
    // can exercise the real "persistent hit" codepath in the executor.
    //
    // Use a guard to ensure this test doesn't pollute other wasm tests.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let vs_bytes = assemble_vs_pos_only();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Vertex, &vs_bytes);
    let stream = writer.finish();

    // First run: persistent get -> miss, translate, then persistent put.
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    // Reset executor (clears in-memory caches) and run again: should hit persistent cache.
    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let open_calls = read_f64(&store, "openCalls") as u32;

    // One miss + one hit.
    assert_eq!(get_calls, 2);
    // Only the first run should persist.
    assert_eq!(put_calls, 1);
    // The cache wrapper is recreated on reset, so it should reopen the persistent cache.
    assert_eq!(open_calls, 2);
}
