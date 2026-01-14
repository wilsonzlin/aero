#![cfg(target_arch = "wasm32")]

use crate::common;
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use js_sys::{Array, Map, Object, Reflect};
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

        Self {
            prev_api,
            prev_store,
        }
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
    Reflect::set(
        &store,
        &JsValue::from_str("getCalls"),
        &JsValue::from_f64(0.0),
    )
    .unwrap();
    Reflect::set(
        &store,
        &JsValue::from_str("putCalls"),
        &JsValue::from_f64(0.0),
    )
    .unwrap();
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
            // Ensure our shader cache key derivation is sensitive to translator semantic changes.
            let version = Reflect::get(&flags, &JsValue::from_str("d3d9TranslatorVersion"))
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u32);
            assert_eq!(
                version,
                Some(aero_d3d9::runtime::D3D9_TRANSLATOR_CACHE_VERSION),
                "expected d3d9TranslatorVersion flag to be present for cache invalidation"
            );

            // Ensure capsHash is passed through so the persistent cache can key by device fingerprint.
            let caps_hash = Reflect::get(&flags, &JsValue::from_str("capsHash"))
                .ok()
                .and_then(|v| v.as_string());
            assert!(
                caps_hash.is_some(),
                "expected capsHash to be present in shader translation flags"
            );

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
        Reflect::set(
            &inner,
            &JsValue::from_str("getShader"),
            get_fn.as_ref().unchecked_ref(),
        )
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
        Reflect::set(
            &inner,
            &JsValue::from_str("putShader"),
            put_fn.as_ref().unchecked_ref(),
        )
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

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_reflection_schema_mismatch() {
    // Install a minimal stub `AeroPersistentGpuCache` so the Rust wasm persistent cache wrapper
    // can exercise invalidation + retry behavior when cached reflection metadata is stale.
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

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(
        keys.length(),
        1,
        "expected a single shader entry to be persisted"
    );
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    // Confirm the reflection includes a non-zero schemaVersion and then delete it to simulate an
    // older cached reflection blob.
    let reflection =
        Reflect::get(&cached, &JsValue::from_str("reflection")).expect("get cached.reflection");
    let reflection_obj: Object = reflection
        .clone()
        .dyn_into()
        .expect("cached.reflection should be an object");
    let schema_version_before = Reflect::get(&reflection, &JsValue::from_str("schemaVersion"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_ne!(
        schema_version_before, 0,
        "expected persisted reflection to contain schemaVersion"
    );
    let _ = Reflect::delete_property(&reflection_obj, &JsValue::from_str("schemaVersion"));

    // Second run after reset: persistent get -> hit with stale reflection, invalidate, retranslate,
    // then persistent put with updated reflection schema.
    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;

    // 1st run: get+put
    // 2nd run: get (hit stale) + delete + get (miss) + put (retranslated)
    assert_eq!(
        get_calls, 3,
        "expected invalidate+retry to re-check persistence"
    );
    assert_eq!(
        put_calls, 2,
        "expected retranslation to be persisted after invalidation"
    );
    assert_eq!(delete_calls, 1, "expected stale cached entry to be deleted");

    let cached_after = map.get(&key);
    let reflection_after = Reflect::get(&cached_after, &JsValue::from_str("reflection"))
        .expect("get cached_after.reflection");
    let schema_version_after = Reflect::get(&reflection_after, &JsValue::from_str("schemaVersion"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_eq!(
        schema_version_after, schema_version_before,
        "expected retranslation to restore schemaVersion after invalidation"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_reflection_stage_mismatch() {
    // Install a minimal stub `AeroPersistentGpuCache` so the Rust wasm persistent cache wrapper
    // can exercise invalidation + retry behavior when cached reflection metadata is stale.
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

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(
        keys.length(),
        1,
        "expected a single shader entry to be persisted"
    );
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    // Corrupt the cached reflection stage to simulate a stale entry (e.g. old schema / bug).
    let reflection =
        Reflect::get(&cached, &JsValue::from_str("reflection")).expect("get cached.reflection");
    let reflection_obj: Object = reflection
        .clone()
        .dyn_into()
        .expect("cached.reflection should be an object");
    Reflect::set(
        &reflection_obj,
        &JsValue::from_str("stage"),
        &JsValue::from_str("pixel"),
    )
    .expect("set reflection.stage");

    // Second run after reset: persistent get -> hit with stale reflection, invalidate, retranslate,
    // then persistent put with updated reflection.
    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;

    // 1st run: get+put
    // 2nd run: get (hit stale) + delete + get (miss) + put (retranslated)
    assert_eq!(
        get_calls, 3,
        "expected invalidate+retry to re-check persistence"
    );
    assert_eq!(
        put_calls, 2,
        "expected retranslation to be persisted after invalidation"
    );
    assert_eq!(delete_calls, 1, "expected stale cached entry to be deleted");

    let cached_after = map.get(&key);
    let reflection_after =
        Reflect::get(&cached_after, &JsValue::from_str("reflection")).expect("get reflection");
    let stage_after = Reflect::get(&reflection_after, &JsValue::from_str("stage"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert_eq!(
        stage_after, "vertex",
        "expected retranslation to restore correct stage metadata"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_reflection_entry_point_mismatch() {
    // Install a minimal stub `AeroPersistentGpuCache` so the Rust wasm persistent cache wrapper
    // can exercise invalidation + retry behavior when cached reflection metadata is stale.
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

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(
        keys.length(),
        1,
        "expected a single shader entry to be persisted"
    );
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    // Corrupt the cached reflection entry point to simulate a stale entry (e.g. old schema / bug).
    // Keep stage intact so the executor must validate stage+entry point consistency.
    let reflection =
        Reflect::get(&cached, &JsValue::from_str("reflection")).expect("get cached.reflection");
    let reflection_obj: Object = reflection
        .clone()
        .dyn_into()
        .expect("cached.reflection should be an object");
    Reflect::set(
        &reflection_obj,
        &JsValue::from_str("entryPoint"),
        &JsValue::from_str("fs_main"),
    )
    .expect("set reflection.entryPoint");

    // Second run after reset: persistent get -> hit with stale reflection, invalidate, retranslate,
    // then persistent put with updated reflection.
    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;

    // 1st run: get+put
    // 2nd run: get (hit stale) + delete + get (miss) + put (retranslated)
    assert_eq!(
        get_calls, 3,
        "expected invalidate+retry to re-check persistence"
    );
    assert_eq!(
        put_calls, 2,
        "expected retranslation to be persisted after invalidation"
    );
    assert_eq!(delete_calls, 1, "expected stale cached entry to be deleted");

    let cached_after = map.get(&key);
    let reflection_after =
        Reflect::get(&cached_after, &JsValue::from_str("reflection")).expect("get reflection");
    let entry_point_after = Reflect::get(&reflection_after, &JsValue::from_str("entryPoint"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert_eq!(
        entry_point_after, "vs_main",
        "expected retranslation to restore correct entry point metadata"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_does_not_invalidate_cache_on_guest_stage_mismatch() {
    // If the guest passes a mismatched stage in CREATE_SHADER_DXBC, we should return an error but
    // not delete the cached artifact (since it may be valid for later correct calls).
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
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("baseline shader create succeeds");

    // Reset executor (clears in-memory caches) and attempt to create the same DXBC with the wrong
    // stage. This should be treated as a guest bug and must not invalidate persistence.
    exec.reset();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &vs_bytes);
    let bad_stream = writer.finish();
    let err = exec
        .execute_cmd_stream_for_context_async(0, &bad_stream)
        .await
        .expect_err("stage mismatch should be rejected");
    assert!(
        matches!(err, AerogpuD3d9Error::ShaderStageMismatch { .. }),
        "expected ShaderStageMismatch, got {err:?}"
    );

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;

    // First run: get miss + put
    // Second run: persistent hit (get) but stage mismatch should not delete or re-put.
    assert_eq!(get_calls, 2, "expected one persistent lookup per run");
    assert_eq!(
        put_calls, 1,
        "expected no retranslation on guest stage mismatch"
    );
    assert_eq!(
        delete_calls, 0,
        "expected cached entry to survive guest stage mismatch"
    );
}
