#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use std::rc::Rc;

use aero_d3d11::runtime::{
    PersistedBinding, PersistedShaderArtifact, PersistedShaderStage, ShaderCache,
    ShaderCacheSource, ShaderTranslationFlags, D3D11_TRANSLATOR_CACHE_VERSION,
};
use js_sys::{Map, Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
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
    // Backing store shared across cache instances within a single wasm-bindgen-test session.
    let store = Object::new();
    let map = Map::new();
    Reflect::set(&store, &JsValue::from_str("map"), &map).unwrap();
    Reflect::set(
        &store,
        &JsValue::from_str("openCalls"),
        &JsValue::from_f64(0.0),
    )
    .unwrap();
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

    let api = Object::new();

    // computeShaderCacheKey(dxbc, flags) -> string
    // Include d3d11TranslatorVersion so bumping it invalidates keys.
    let compute_fn = Closure::<dyn FnMut(js_sys::Uint8Array, JsValue) -> JsValue>::wrap(Box::new(
        move |_dxbc: js_sys::Uint8Array, flags: JsValue| -> JsValue {
            let version = Reflect::get(&flags, &JsValue::from_str("d3d11TranslatorVersion"))
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u32);
            assert_eq!(
                version,
                Some(D3D11_TRANSLATOR_CACHE_VERSION),
                "expected d3d11TranslatorVersion flag to be present for cache invalidation"
            );

            // Ensure capsHash can be present without affecting the content-hash portion of the key.
            let _caps_hash = Reflect::get(&flags, &JsValue::from_str("capsHash"))
                .ok()
                .and_then(|v| v.as_string());

            let flags_s = js_sys::JSON::stringify(&flags)
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "null".to_string());
            JsValue::from_str(&format!("test-key-{flags_s}"))
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

fn make_artifact(tag: &str) -> PersistedShaderArtifact {
    PersistedShaderArtifact {
        wgsl: format!("// wgsl {tag}"),
        stage: PersistedShaderStage::Vertex,
        bindings: Vec::<PersistedBinding>::new(),
        vs_input_signature: Vec::new(),
    }
}

#[wasm_bindgen_test(async)]
async fn write_then_reload_cache_reads_persistent_entry() {
    let (api, store) = make_persistent_cache_stub();
    let _guard = PersistentCacheApiGuard::install(&api, &store);

    // Use a stable fake DXBC blob.
    let dxbc = b"fake dxbc";
    let flags = ShaderTranslationFlags::new(Some("caps".to_string()));

    let translate_calls = Rc::new(Cell::new(0u32));

    // First cache instance: should translate + persist.
    {
        let mut cache = ShaderCache::new();
        let translate_calls_cb = translate_calls.clone();
        let (_artifact, source) = cache
            .get_or_translate_with_source(dxbc, flags.clone(), move || {
                let translate_calls_cb = translate_calls_cb.clone();
                async move {
                    translate_calls_cb.set(translate_calls_cb.get() + 1);
                    Ok(make_artifact("first"))
                }
            })
            .await
            .unwrap();
        assert_eq!(source, ShaderCacheSource::Translated);
    }

    // Second cache instance: should hit persistent storage (not in-memory).
    {
        let mut cache = ShaderCache::new();
        let (_artifact, source) = cache
            .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
                panic!("expected persistent hit; translation should not run");
            })
            .await
            .unwrap();
        assert_eq!(source, ShaderCacheSource::Persistent);
    }

    assert_eq!(translate_calls.get(), 1);

    // Sanity-check that the stub persistent store saw at least one put/get call.
    let get_calls = Reflect::get(&store, &JsValue::from_str("getCalls"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let put_calls = Reflect::get(&store, &JsValue::from_str("putCalls"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(get_calls >= 2.0);
    assert!(put_calls >= 1.0);
}

#[wasm_bindgen_test(async)]
async fn cache_invalidates_when_translator_version_changes() {
    // A stub that incorporates d3d11TranslatorVersion into the key.
    let store = Object::new();
    let map = Map::new();
    Reflect::set(&store, &JsValue::from_str("map"), &map).unwrap();

    let api = Object::new();
    let compute_fn = Closure::<dyn FnMut(js_sys::Uint8Array, JsValue) -> JsValue>::wrap(Box::new(
        move |_dxbc: js_sys::Uint8Array, flags: JsValue| -> JsValue {
            let version = Reflect::get(&flags, &JsValue::from_str("d3d11TranslatorVersion"))
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u32)
                .unwrap_or(0);
            JsValue::from_str(&format!("test-key-v{version}"))
        },
    ));
    Reflect::set(
        &api,
        &JsValue::from_str("computeShaderCacheKey"),
        compute_fn.as_ref().unchecked_ref(),
    )
    .unwrap();
    compute_fn.forget();

    let persistent_gpu_cache = Object::new();
    let open_map = map.clone();
    let open_fn = Closure::<dyn FnMut() -> JsValue>::wrap(Box::new(move || -> JsValue {
        let inner = Object::new();
        let get_map = open_map.clone();
        let get_fn = Closure::<dyn FnMut(String) -> JsValue>::wrap(Box::new(move |key: String| {
            get_map.get(&JsValue::from_str(&key))
        }));
        Reflect::set(
            &inner,
            &JsValue::from_str("getShader"),
            get_fn.as_ref().unchecked_ref(),
        )
        .unwrap();
        get_fn.forget();

        let put_map = open_map.clone();
        let put_fn =
            Closure::<dyn FnMut(String, JsValue) -> JsValue>::wrap(Box::new(move |key, value| {
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

        let del_map = open_map.clone();
        let del_fn = Closure::<dyn FnMut(String) -> JsValue>::wrap(Box::new(move |key| {
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

    let api_val: JsValue = api.into();
    let store_val: JsValue = store.into();
    let _guard = PersistentCacheApiGuard::install(&api_val, &store_val);

    let dxbc = b"fake dxbc";

    // Write with version N.
    let mut flags_v1 = ShaderTranslationFlags::default();
    flags_v1.caps_hash = Some("caps".to_string());
    flags_v1.d3d11_translator_version = 1;
    let mut cache = ShaderCache::new();
    let (_artifact, source) = cache
        .get_or_translate_with_source(dxbc, flags_v1.clone(), move || async move {
            Ok(make_artifact("v1"))
        })
        .await
        .unwrap();
    assert_eq!(source, ShaderCacheSource::Translated);

    // Read with version N+1 -> miss (new key), triggers translate.
    let mut flags_v2 = flags_v1.clone();
    flags_v2.d3d11_translator_version = 2;
    let (_artifact2, source2) = cache
        .get_or_translate_with_source(dxbc, flags_v2.clone(), move || async move {
            Ok(make_artifact("v2"))
        })
        .await
        .unwrap();
    assert_eq!(
        source2,
        ShaderCacheSource::Translated,
        "version bump should result in cache miss + translation"
    );
}
