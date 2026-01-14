use crate::common;
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, AerogpuD3d9ExecutorConfig};
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
    // D3D9 SM2/SM3 encodes the *total* instruction length in DWORD tokens (including the opcode
    // token) in bits 24..27.
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24);
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

fn assemble_ps_texld_s0() -> Vec<u8> {
    // ps_2_0: texld r0, t0, s0; mov oC0, r0; end
    let mut out = vec![0xFFFF0200];
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    to_bytes(&out)
}

fn assemble_ps_texld_cube_s0() -> Vec<u8> {
    // ps_2_0 with explicit cube sampler declaration for s0:
    //   dcl_cube s0
    //   texld r0, t0, s0
    //   mov oC0, r0
    //   end
    let mut out = vec![0xFFFF0200];
    let decl_token = 3u32 << 27; // D3DSAMPLER_TEXTURE_TYPE_CUBE
    out.extend(enc_inst(0x001F, &[decl_token, enc_dst(10, 0, 0)]));
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    to_bytes(&out)
}

fn assemble_ps_unknown_opcode_fallback() -> Vec<u8> {
    // ps_2_0 with an unknown opcode to force SM3->legacy fallback:
    //   <unknown>
    //   mov oC0, c0
    //   end
    let mut out = vec![0xFFFF0200];
    out.extend(enc_inst(0x1234, &[]));
    out.extend(enc_inst(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)], // oC0 = c0
    ));
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
async fn d3d9_executor_uses_persistent_shader_cache_for_legacy_fallback_shaders() {
    // Some shaders still use the SM3->legacy fallback translation path. Ensure the persistent cache
    // hit validation accepts the legacy WGSL (i.e. does not thrash by invalidating every hit).
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_unknown_opcode_fallback();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    // First run: persistent get -> miss, translate (legacy fallback), then persistent put.
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    // Reset executor (clears in-memory caches) and run again: should hit persistent cache without
    // invalidation.
    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;

    assert_eq!(get_calls, 2, "expected one miss + one persistent hit");
    assert_eq!(put_calls, 1, "expected only the first run to persist");
    assert_eq!(
        delete_calls, 0,
        "expected no invalidation on persistent hit"
    );
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
async fn d3d9_executor_retranslates_on_persisted_reflection_malformed() {
    // Cached reflection is stored as JSON. If it becomes malformed (wrong type / not matching the
    // expected schema), the executor should invalidate and retranslate rather than failing later.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    // Replace the reflection blob with an unexpected type (string).
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("reflection"),
        &JsValue::from_str("not a reflection object"),
    )
    .expect("set cached.reflection");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
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
async fn d3d9_executor_retranslates_on_persisted_reflection_used_samplers_mask_mismatch() {
    // Corrupting usedSamplersMask can cause missing texture flushes or bind group layouts that
    // don't match the cached WGSL. The executor should detect this on persistent cache hit,
    // invalidate+retry once, and then persist the corrected reflection.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);

    let reflection =
        Reflect::get(&cached, &JsValue::from_str("reflection")).expect("get cached.reflection");
    let used_mask_before = Reflect::get(&reflection, &JsValue::from_str("usedSamplersMask"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_eq!(used_mask_before, 1, "expected shader to use s0");

    // Corrupt usedSamplersMask.
    let reflection_obj: Object = reflection
        .clone()
        .dyn_into()
        .expect("cached.reflection should be an object");
    Reflect::set(
        &reflection_obj,
        &JsValue::from_str("usedSamplersMask"),
        &JsValue::from_f64(0.0),
    )
    .expect("set reflection.usedSamplersMask");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let reflection_after =
        Reflect::get(&cached_after, &JsValue::from_str("reflection")).expect("get reflection");
    let used_mask_after = Reflect::get(&reflection_after, &JsValue::from_str("usedSamplersMask"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_eq!(
        used_mask_after, used_mask_before,
        "expected retranslation to restore usedSamplersMask"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_reflection_sampler_dim_key_mismatch() {
    // Corrupting samplerDimKey can cause bind group layouts that don't match the cached WGSL
    // (e.g. texture_cube vs texture_2d). The executor should detect this and invalidate+retry.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_cube_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);

    let reflection =
        Reflect::get(&cached, &JsValue::from_str("reflection")).expect("get cached.reflection");
    let dim_key_before = Reflect::get(&reflection, &JsValue::from_str("samplerDimKey"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_eq!(dim_key_before, 1, "expected shader to use cube sampler s0");

    // Corrupt samplerDimKey.
    let reflection_obj: Object = reflection
        .clone()
        .dyn_into()
        .expect("cached.reflection should be an object");
    Reflect::set(
        &reflection_obj,
        &JsValue::from_str("samplerDimKey"),
        &JsValue::from_f64(0.0),
    )
    .expect("set reflection.samplerDimKey");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let reflection_after =
        Reflect::get(&cached_after, &JsValue::from_str("reflection")).expect("get reflection");
    let dim_key_after = Reflect::get(&reflection_after, &JsValue::from_str("samplerDimKey"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_eq!(
        dim_key_after, dim_key_before,
        "expected retranslation to restore samplerDimKey"
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

fn assemble_vs_dcl_pos_color0_v7() -> Vec<u8> {
    // vs_2_0 with semantic-based DCL declarations:
    //   dcl_position v0
    //   dcl_color0 v7
    //   add r0, v0, v7
    //   mov oPos, r0
    //   end
    let mut out = vec![0xFFFE0200];
    // usage_raw=0 (Position), usage_index=0
    out.extend(enc_inst(0x001F, &[0u32, enc_dst(1, 0, 0)]));
    // usage_raw=10 (Color), usage_index=0
    out.extend(enc_inst(0x001F, &[10u32, enc_dst(1, 7, 0)]));
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),  // r0
            enc_src(1, 0, 0xE4), // v0
            enc_src(1, 7, 0xE4), // v7
        ],
    ));
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    to_bytes(&out)
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_reflection_semantic_locations_corruption() {
    // Corrupting semanticLocations can cause incorrect vertex attribute binding. The executor
    // should detect obviously inconsistent mappings on persistent cache hit and invalidate+retry.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let vs_bytes = assemble_vs_dcl_pos_color0_v7();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Vertex, &vs_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let reflection =
        Reflect::get(&cached, &JsValue::from_str("reflection")).expect("get cached.reflection");
    let uses_semantic_locations =
        Reflect::get(&reflection, &JsValue::from_str("usesSemanticLocations"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    assert!(
        uses_semantic_locations,
        "expected shader to use semantic location remapping"
    );

    let semantic_locations = Reflect::get(&reflection, &JsValue::from_str("semanticLocations"))
        .expect("get reflection.semanticLocations");
    let semantic_locations: Array = semantic_locations
        .dyn_into()
        .expect("semanticLocations should be an array");
    assert!(
        semantic_locations.length() >= 2,
        "expected shader to persist multiple semanticLocations"
    );
    let first = semantic_locations.get(0);
    let second = semantic_locations.get(1);
    let first_loc = Reflect::get(&first, &JsValue::from_str("location"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    let second_loc = Reflect::get(&second, &JsValue::from_str("location"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_ne!(
        first_loc, second_loc,
        "expected semantic location mapping to contain distinct locations"
    );

    // Corrupt the semantic location mapping by forcing a duplicate @location.
    let second_obj: Object = second
        .dyn_into()
        .expect("semanticLocations[1] should be an object");
    Reflect::set(
        &second_obj,
        &JsValue::from_str("location"),
        &JsValue::from_f64(first_loc as f64),
    )
    .expect("set semanticLocations[1].location");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let reflection_after =
        Reflect::get(&cached_after, &JsValue::from_str("reflection")).expect("get reflection");
    let semantic_locations_after =
        Reflect::get(&reflection_after, &JsValue::from_str("semanticLocations"))
            .expect("get reflection_after.semanticLocations");
    let semantic_locations_after: Array = semantic_locations_after
        .dyn_into()
        .expect("semanticLocations_after should be an array");
    assert!(
        semantic_locations_after.length() >= 2,
        "expected semanticLocations to remain populated after retranslation"
    );
    let first_after = semantic_locations_after.get(0);
    let second_after = semantic_locations_after.get(1);
    let first_loc_after = Reflect::get(&first_after, &JsValue::from_str("location"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    let second_loc_after = Reflect::get(&second_after, &JsValue::from_str("location"))
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);
    assert_ne!(
        first_loc_after, second_loc_after,
        "expected retranslation to restore distinct semanticLocations"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_entry_point_mismatch() {
    // Cached WGSL can be stale/corrupt even when reflection metadata looks valid. If the WGSL does
    // not contain the expected entry point, pipeline creation would fail later. Detect this on
    // persistent cache hit and invalidate+retry.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("fn vs_main("),
        "expected cached WGSL to contain vs_main entry point"
    );

    let wgsl_corrupt = wgsl_before.replace("fn vs_main", "fn vs_main_corrupt");
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_after.contains("fn vs_main(") && !wgsl_after.contains("vs_main_corrupt"),
        "expected retranslation to restore the correct WGSL entry point"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_entry_point_stage_mismatch() {
    // Cached WGSL may contain both @vertex and fn vs_main, but the *pairing* can still be wrong
    // (e.g. a fragment entry point named vs_main plus a different vertex function). Detect this on
    // persistent cache hit and invalidate+retry to avoid later pipeline creation failures.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@vertex\nfn vs_main"),
        "expected cached WGSL to contain @vertex vs_main entry point"
    );

    // Rename the real vertex entry point and add a fragment entry point named `vs_main`.
    // This still contains both '@vertex' and 'fn vs_main', but the pairing is wrong.
    let mut wgsl_corrupt = wgsl_before.replace("@vertex\nfn vs_main", "@vertex\nfn vs_main_actual");
    wgsl_corrupt.push_str(
        "\n@fragment\nfn vs_main() -> @location(0) vec4<f32> {\n  return vec4<f32>(0.0);\n}\n",
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_after.contains("@vertex\nfn vs_main") && !wgsl_after.contains("vs_main_actual"),
        "expected retranslation to restore correct @vertex vs_main entry point"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_accepts_persisted_wgsl_entry_point_one_line_formatting() {
    // Some WGSL writers format stage attributes on the same line as the entrypoint:
    //   `@vertex fn vs_main(...)`
    //
    // The persistent cache should accept such formatting (it's still semantically the same entry
    // point), and must not thrash by invalidating+retranslating a valid cached module.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@vertex\nfn vs_main"),
        "expected cached WGSL to contain @vertex vs_main entry point"
    );

    let wgsl_one_line = wgsl_before.replace("@vertex\nfn vs_main", "@vertex fn vs_main");
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_one_line),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 2, "expected persistent cache hit");
    assert_eq!(put_calls, 1, "expected no retranslation/persist");
    assert_eq!(delete_calls, 0, "expected no invalidation");
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_accepts_persisted_wgsl_entry_point_with_comment_line() {
    // Comments are whitespace in WGSL and may appear between the stage attribute and `fn` line. The
    // persistent cache hit validator should accept this formatting and avoid invalidating a valid
    // cached module.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@vertex\nfn vs_main"),
        "expected cached WGSL to contain @vertex vs_main entry point"
    );

    let wgsl_comment = wgsl_before.replace(
        "@vertex\nfn vs_main",
        "@vertex\n// entry point comment\nfn vs_main",
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_comment),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 2, "expected persistent cache hit");
    assert_eq!(put_calls, 1, "expected no retranslation/persist");
    assert_eq!(delete_calls, 0, "expected no invalidation");
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_accepts_persisted_wgsl_multiline_binding_attributes() {
    // WGSL allows attributes to be split across multiple lines. Ensure our persistent-hit validator
    // accepts this formatting and does not thrash by invalidating every persistent hit.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@group(0) @binding(0) var<uniform> constants:")
            && wgsl_before.contains("@group(2) @binding(0) var tex0")
            && wgsl_before.contains("@group(2) @binding(1) var samp0"),
        "expected cached WGSL to contain constants and sampler bindings"
    );

    // Split attributes across lines but keep the binding contract intact.
    let wgsl_multiline = wgsl_before
        .replace(
            "@group(0) @binding(0) var<uniform> constants:",
            "@group(0)\n@binding(0)\nvar<uniform> constants:",
        )
        .replace(
            "@group(2) @binding(0) var tex0",
            "@group(2)\n@binding(0)\nvar tex0",
        )
        .replace(
            "@group(2) @binding(1) var samp0",
            "@group(2)\n@binding(1)\nvar samp0",
        );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_multiline),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 2, "expected persistent cache hit");
    assert_eq!(put_calls, 1, "expected no retranslation/persist");
    assert_eq!(delete_calls, 0, "expected no invalidation");
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_wgpu_validation_error() {
    // Even if high-level string checks pass, cached WGSL can still be corrupt/invalid. Ensure we
    // catch this via a wgpu validation scope on persistent hits.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@vertex\nfn vs_main"),
        "expected cached WGSL to contain @vertex vs_main entry point"
    );

    let wgsl_corrupt = format!("{wgsl_before}\nthis is not wgsl\n");
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        !wgsl_after.contains("this is not wgsl"),
        "expected retranslation to restore valid WGSL"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_sampler_binding_mismatch() {
    // Cached WGSL may be corrupt even when reflection metadata looks consistent. If sampler binding
    // numbers are wrong, pipeline creation will fail later due to bind group layout mismatch.
    // Detect this on persistent cache hit and invalidate+retry.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@binding(0) var tex0")
            && wgsl_before.contains("@binding(1) var samp0"),
        "expected cached WGSL to contain sampler declarations for s0"
    );

    // Corrupt tex0 binding number so it no longer matches the executor's bind group layout
    // contract (binding = 2*s).
    let wgsl_corrupt = wgsl_before.replace("@binding(0) var tex0", "@binding(4) var tex0");
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_after.contains("@binding(0) var tex0") && !wgsl_after.contains("@binding(4) var tex0"),
        "expected retranslation to restore correct sampler bindings"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_sampler_binding_mismatch_multiline_attrs() {
    // Same as `*_sampler_binding_mismatch`, but with `@group/@binding` split across lines to ensure
    // the cache-hit WGSL validator can't be bypassed by formatting changes.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@group(2) @binding(0) var tex0"),
        "expected cached WGSL to contain a tex0 declaration for PS sampler group"
    );

    let wgsl_corrupt = wgsl_before.replace(
        "@group(2) @binding(0) var tex0",
        "@group(2)\n@binding(4) var tex0",
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_half_pixel_uniform_binding_mismatch() {
    // When half-pixel-center support is enabled, vertex shaders include an extra uniform binding
    // in group(3). If the cached WGSL is corrupt (wrong group/binding), pipeline creation would
    // fail later. Detect this on persistent cache hit and invalidate+retry.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless_with_config(AerogpuD3d9ExecutorConfig {
        half_pixel_center: true,
    })
    .await
    {
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("var<uniform> half_pixel"),
        "expected cached WGSL to contain half_pixel uniform binding"
    );

    let wgsl_corrupt = wgsl_before.replace(
        "@group(3) @binding(0) var<uniform> half_pixel",
        "@group(4) @binding(0) var<uniform> half_pixel",
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_after.contains("@group(3) @binding(0) var<uniform> half_pixel")
            && !wgsl_after.contains("@group(4) @binding(0) var<uniform> half_pixel"),
        "expected retranslation to restore correct half_pixel uniform binding"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_constants_binding_mismatch() {
    // Cached WGSL must follow the executor's binding contract for the shared constants uniforms
    // (group(0) binding(0)). If this is corrupted, pipeline creation would fail later; detect
    // it on persistent cache hit and invalidate+retry.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@group(0) @binding(0) var<uniform> constants:"),
        "expected cached WGSL to bind constants at group(0) binding(0)"
    );

    let wgsl_corrupt = wgsl_before.replace(
        "@group(0) @binding(0) var<uniform> constants:",
        "@group(0) @binding(1) var<uniform> constants:",
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_after.contains("@group(0) @binding(0) var<uniform> constants:")
            && !wgsl_after.contains("@group(0) @binding(1) var<uniform> constants:"),
        "expected retranslation to restore correct constants binding"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_constants_binding_mismatch_multiline_attrs() {
    // Same as `*_constants_binding_mismatch`, but with `@group/@binding` split across lines. This
    // ensures the cache-hit WGSL validator doesn't get tricked by formatting changes/corruption.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@group(0) @binding(0) var<uniform> constants:"),
        "expected cached WGSL to bind constants at group(0) binding(0)"
    );

    let wgsl_corrupt = wgsl_before.replace(
        "@group(0) @binding(0) var<uniform> constants:",
        "@group(0)\n@binding(1) var<uniform> constants:",
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_out_of_range_sampler_declaration() {
    // The executor only supports samplers s0..s15. If cached WGSL is corrupted to include
    // declarations for out-of-range samplers, pipeline creation would fail later. Detect this on
    // persistent cache hit and invalidate+retry.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@fragment\nfn fs_main"),
        "expected cached WGSL to contain fs_main entry point"
    );

    // Inject an out-of-range sampler declaration (tex16/samp16).
    let extra = "@group(2) @binding(32) var tex16: texture_2d<f32>;\n@group(2) @binding(33) var samp16: sampler;\n\n";
    let wgsl_corrupt = wgsl_before.replace(
        "@fragment\nfn fs_main",
        &format!("{extra}@fragment\nfn fs_main"),
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        !wgsl_after.contains("tex16") && !wgsl_after.contains("samp16"),
        "expected retranslation to remove out-of-range sampler declarations"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_half_pixel_uniform_in_pixel_shader() {
    // Pixel shaders should never declare the half_pixel uniform. If cached WGSL is corrupted to
    // include it, the executor should invalidate on persistent hit to avoid later pipeline layout
    // errors.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        // Be precise (avoid substring collisions with other identifiers).
        wgsl_before.contains("@group(0) @binding(0) var<uniform> constants:"),
        "expected cached WGSL to contain constants uniform"
    );

    let wgsl_corrupt = wgsl_before.replace(
        "@group(0) @binding(0) var<uniform> constants: Constants;",
        "@group(0) @binding(0) var<uniform> constants: Constants;\n@group(3) @binding(0) var<uniform> half_pixel: vec4<f32>;",
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        !wgsl_after.contains("var<uniform> half_pixel"),
        "expected retranslation to remove unexpected half_pixel uniform"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_unknown_uniform_binding() {
    // Corrupt cached WGSL by injecting an extra uniform binding that the executor does not provide
    // in its bind group layouts. Detect this on persistent cache hit and invalidate+retry.
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
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("@vertex\nfn vs_main"),
        "expected cached WGSL to contain vs_main entry point"
    );

    let extra = "@group(0) @binding(3) var<uniform> extra_uniform: vec4<f32>;\n\n";
    let wgsl_corrupt = wgsl_before.replace(
        "@vertex\nfn vs_main",
        &format!("{extra}@vertex\nfn vs_main"),
    );
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        !wgsl_after.contains("extra_uniform"),
        "expected retranslation to remove unexpected uniform binding"
    );
}

#[wasm_bindgen_test(async)]
async fn d3d9_executor_retranslates_on_persisted_wgsl_sampler_type_mismatch() {
    // If cached WGSL is corrupted to use an unexpected sampler type (e.g. sampler_comparison),
    // pipeline creation would fail later due to bind group layout mismatch. Detect this on
    // persistent cache hit and invalidate+retry.
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let ps_bytes = assemble_ps_texld_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("first shader create succeeds");

    let map: Map = Reflect::get(&store, &JsValue::from_str("map"))
        .expect("get store.map")
        .dyn_into()
        .expect("store.map should be a Map");
    let keys = Array::from(&map.keys());
    assert_eq!(keys.length(), 1, "expected one persisted shader entry");
    let key = keys.get(0);
    let cached = map.get(&key);
    assert!(
        !cached.is_undefined() && !cached.is_null(),
        "expected persisted cache entry to exist"
    );

    let wgsl_before = Reflect::get(&cached, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_before.contains("var samp0: sampler;"),
        "expected cached WGSL to declare samp0 as a filtering sampler"
    );

    let wgsl_corrupt = wgsl_before.replace("var samp0: sampler;", "var samp0: sampler_comparison;");
    let cached_obj: Object = cached
        .clone()
        .dyn_into()
        .expect("cached entry should be an object");
    Reflect::set(
        &cached_obj,
        &JsValue::from_str("wgsl"),
        &JsValue::from_str(&wgsl_corrupt),
    )
    .expect("set cached.wgsl");

    exec.reset();
    exec.execute_cmd_stream_for_context_async(0, &stream)
        .await
        .expect("second shader create succeeds");

    let get_calls = read_f64(&store, "getCalls") as u32;
    let put_calls = read_f64(&store, "putCalls") as u32;
    let delete_calls = read_f64(&store, "deleteCalls") as u32;
    assert_eq!(get_calls, 3, "expected invalidate+retry after mismatch");
    assert_eq!(put_calls, 2, "expected corrected shader to be persisted");
    assert_eq!(
        delete_calls, 1,
        "expected corrupted cached entry to be deleted"
    );

    let cached_after = map.get(&key);
    let wgsl_after = Reflect::get(&cached_after, &JsValue::from_str("wgsl"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    assert!(
        wgsl_after.contains("var samp0: sampler;") && !wgsl_after.contains("sampler_comparison"),
        "expected retranslation to restore samp0 sampler type"
    );
}
