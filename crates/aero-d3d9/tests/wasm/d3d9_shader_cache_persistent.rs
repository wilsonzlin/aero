#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use std::rc::Rc;

use aero_d3d9::runtime::{
    PersistedShaderArtifact, ShaderCache, ShaderCacheSource, ShaderTranslationFlags,
};
use js_sys::{Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

struct PersistentCacheApiGuard {
    prev: Option<JsValue>,
}

impl PersistentCacheApiGuard {
    fn install(api: &JsValue) -> Self {
        let global = js_sys::global();
        let key = JsValue::from_str("AeroPersistentGpuCache");
        let prev = Reflect::get(&global, &key)
            .ok()
            .and_then(|v| (!v.is_undefined()).then_some(v));
        Reflect::set(&global, &key, api).unwrap();
        Self { prev }
    }
}

impl Drop for PersistentCacheApiGuard {
    fn drop(&mut self) {
        let global = js_sys::global();
        let key = JsValue::from_str("AeroPersistentGpuCache");
        match &self.prev {
            Some(prev) => {
                let _ = Reflect::set(&global, &key, prev);
            }
            None => {
                let _ = Reflect::delete_property(&global, &key);
            }
        }
    }
}

fn make_flags() -> ShaderTranslationFlags {
    ShaderTranslationFlags {
        half_pixel_center: false,
        caps_hash: None,
    }
}

fn make_artifact(tag: &str) -> PersistedShaderArtifact {
    PersistedShaderArtifact {
        wgsl: format!("// wgsl {tag}"),
        reflection: serde_json::Value::Null,
    }
}

#[wasm_bindgen_test(async)]
async fn persistent_cache_opens_once_and_reports_sources() {
    let open_calls = Rc::new(Cell::new(0u32));
    let get_calls = Rc::new(Cell::new(0u32));
    let put_calls = Rc::new(Cell::new(0u32));
    let translate_calls = Rc::new(Cell::new(0u32));

    let api = Object::new();

    // computeShaderCacheKey: return a stable string key.
    let compute =
        Closure::wrap(
            Box::new(move |_dxbc: JsValue, _flags: JsValue| JsValue::from_str("test-key"))
                as Box<dyn FnMut(JsValue, JsValue) -> JsValue>,
        );
    Reflect::set(
        &api,
        &JsValue::from_str("computeShaderCacheKey"),
        compute.as_ref(),
    )
    .unwrap();
    compute.forget();

    // PersistentGpuCache.open: return an inner object with get/put/delete.
    let persistent = Object::new();
    let open_calls_cb = open_calls.clone();
    let get_calls_cb = get_calls.clone();
    let put_calls_cb = put_calls.clone();

    let open = Closure::wrap(Box::new(move || {
        open_calls_cb.set(open_calls_cb.get() + 1);

        let inner = Object::new();

        let get_calls_inner = get_calls_cb.clone();
        let get_shader = Closure::wrap(Box::new(move |_key: JsValue| {
            get_calls_inner.set(get_calls_inner.get() + 1);
            // Miss: return null/undefined.
            JsValue::NULL
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        Reflect::set(&inner, &JsValue::from_str("getShader"), get_shader.as_ref()).unwrap();
        get_shader.forget();

        let put_calls_inner = put_calls_cb.clone();
        let put_shader = Closure::wrap(Box::new(move |_key: JsValue, _value: JsValue| {
            put_calls_inner.set(put_calls_inner.get() + 1);
            JsValue::UNDEFINED
        }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
        Reflect::set(&inner, &JsValue::from_str("putShader"), put_shader.as_ref()).unwrap();
        put_shader.forget();

        let delete_shader =
            Closure::wrap(Box::new(move |_key: JsValue| JsValue::UNDEFINED)
                as Box<dyn FnMut(JsValue) -> JsValue>);
        Reflect::set(
            &inner,
            &JsValue::from_str("deleteShader"),
            delete_shader.as_ref(),
        )
        .unwrap();
        delete_shader.forget();

        inner.into()
    }) as Box<dyn FnMut() -> JsValue>);
    Reflect::set(&persistent, &JsValue::from_str("open"), open.as_ref()).unwrap();
    open.forget();

    Reflect::set(
        &api,
        &JsValue::from_str("PersistentGpuCache"),
        persistent.as_ref(),
    )
    .unwrap();

    let _guard = PersistentCacheApiGuard::install(&api.into());

    let mut cache = ShaderCache::new();
    let dxbc = b"fake dxbc";
    let flags = make_flags();

    let translate_calls_cb = translate_calls.clone();
    let (artifact, source) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || {
            let translate_calls_cb = translate_calls_cb.clone();
            async move {
                translate_calls_cb.set(translate_calls_cb.get() + 1);
                Ok(make_artifact("translated"))
            }
        })
        .await
        .unwrap();

    assert_eq!(source, ShaderCacheSource::Translated);
    assert_eq!(artifact.wgsl, "// wgsl translated");
    assert_eq!(open_calls.get(), 1);
    assert_eq!(get_calls.get(), 1);
    assert_eq!(put_calls.get(), 1);
    assert_eq!(translate_calls.get(), 1);

    let (_artifact2, source2) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
            panic!("second call should be served from in-memory cache");
        })
        .await
        .unwrap();

    assert_eq!(source2, ShaderCacheSource::Memory);
    // Importantly: no additional open/get/put calls after the in-memory hit.
    assert_eq!(open_calls.get(), 1);
    assert_eq!(get_calls.get(), 1);
    assert_eq!(put_calls.get(), 1);
    assert_eq!(translate_calls.get(), 1);
}

#[wasm_bindgen_test(async)]
async fn persistence_is_disabled_after_open_error() {
    let open_calls = Rc::new(Cell::new(0u32));

    let api = Object::new();

    let compute =
        Closure::wrap(
            Box::new(move |_dxbc: JsValue, _flags: JsValue| JsValue::from_str("test-key"))
                as Box<dyn FnMut(JsValue, JsValue) -> JsValue>,
        );
    Reflect::set(
        &api,
        &JsValue::from_str("computeShaderCacheKey"),
        compute.as_ref(),
    )
    .unwrap();
    compute.forget();

    let persistent = Object::new();
    let open_calls_cb = open_calls.clone();
    let open = Closure::wrap(Box::new(move || {
        open_calls_cb.set(open_calls_cb.get() + 1);
        // Simulate IndexedDB/OPFS/permission failures by returning a rejected promise.
        js_sys::Promise::reject(&JsValue::from_str("open failed")).into()
    }) as Box<dyn FnMut() -> JsValue>);
    Reflect::set(&persistent, &JsValue::from_str("open"), open.as_ref()).unwrap();
    open.forget();
    Reflect::set(
        &api,
        &JsValue::from_str("PersistentGpuCache"),
        persistent.as_ref(),
    )
    .unwrap();

    let _guard = PersistentCacheApiGuard::install(&api.into());

    let mut cache = ShaderCache::new();
    let dxbc = b"fake dxbc";
    let flags = make_flags();

    let (_artifact, source) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
            Ok(make_artifact("translated"))
        })
        .await
        .unwrap();
    assert_eq!(source, ShaderCacheSource::Translated);
    assert_eq!(open_calls.get(), 1);

    // Subsequent lookups should not attempt to reopen persistence.
    let (_artifact2, source2) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
            panic!("second call should be served from in-memory cache");
        })
        .await
        .unwrap();
    assert_eq!(source2, ShaderCacheSource::Memory);
    assert_eq!(open_calls.get(), 1);
}

#[wasm_bindgen_test(async)]
async fn persistence_is_disabled_after_key_derivation_error() {
    let open_calls = Rc::new(Cell::new(0u32));

    let api = Object::new();

    // computeShaderCacheKey throws => should disable persistence without hard failing translation.
    let compute = Closure::wrap(Box::new(move |_dxbc: JsValue, _flags: JsValue| {
        wasm_bindgen::throw_str("missing api");
    }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
    Reflect::set(
        &api,
        &JsValue::from_str("computeShaderCacheKey"),
        compute.as_ref(),
    )
    .unwrap();
    compute.forget();

    let persistent = Object::new();
    let open_calls_cb = open_calls.clone();
    let open = Closure::wrap(Box::new(move || {
        open_calls_cb.set(open_calls_cb.get() + 1);
        Object::new().into()
    }) as Box<dyn FnMut() -> JsValue>);
    Reflect::set(&persistent, &JsValue::from_str("open"), open.as_ref()).unwrap();
    open.forget();
    Reflect::set(
        &api,
        &JsValue::from_str("PersistentGpuCache"),
        persistent.as_ref(),
    )
    .unwrap();

    let _guard = PersistentCacheApiGuard::install(&api.into());

    let mut cache = ShaderCache::new();
    let dxbc = b"fake dxbc";
    let flags = make_flags();

    let (_artifact, source) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
            Ok(make_artifact("translated"))
        })
        .await
        .unwrap();
    assert_eq!(source, ShaderCacheSource::Translated);
    // Because key derivation failed, persistence should be disabled without attempting to open.
    assert_eq!(open_calls.get(), 0);
}

#[wasm_bindgen_test(async)]
async fn persistent_hit_reports_source_and_populates_in_memory_cache() {
    let open_calls = Rc::new(Cell::new(0u32));
    let get_calls = Rc::new(Cell::new(0u32));
    let put_calls = Rc::new(Cell::new(0u32));

    let api = Object::new();

    let compute =
        Closure::wrap(
            Box::new(move |_dxbc: JsValue, _flags: JsValue| JsValue::from_str("test-key"))
                as Box<dyn FnMut(JsValue, JsValue) -> JsValue>,
        );
    Reflect::set(
        &api,
        &JsValue::from_str("computeShaderCacheKey"),
        compute.as_ref(),
    )
    .unwrap();
    compute.forget();

    let persistent = Object::new();
    let open_calls_cb = open_calls.clone();
    let get_calls_cb = get_calls.clone();
    let put_calls_cb = put_calls.clone();

    let cached_val =
        serde_wasm_bindgen::to_value(&make_artifact("persistent")).expect("serialize artifact");

    let open = Closure::wrap(Box::new(move || {
        open_calls_cb.set(open_calls_cb.get() + 1);

        let inner = Object::new();

        let cached_val_inner = cached_val.clone();
        let get_calls_inner = get_calls_cb.clone();
        let get_shader = Closure::wrap(Box::new(move |_key: JsValue| {
            get_calls_inner.set(get_calls_inner.get() + 1);
            cached_val_inner.clone()
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        Reflect::set(&inner, &JsValue::from_str("getShader"), get_shader.as_ref()).unwrap();
        get_shader.forget();

        let put_calls_inner = put_calls_cb.clone();
        let put_shader = Closure::wrap(Box::new(move |_key: JsValue, _value: JsValue| {
            put_calls_inner.set(put_calls_inner.get() + 1);
            JsValue::UNDEFINED
        }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
        Reflect::set(&inner, &JsValue::from_str("putShader"), put_shader.as_ref()).unwrap();
        put_shader.forget();

        let delete_shader =
            Closure::wrap(Box::new(move |_key: JsValue| JsValue::UNDEFINED)
                as Box<dyn FnMut(JsValue) -> JsValue>);
        Reflect::set(
            &inner,
            &JsValue::from_str("deleteShader"),
            delete_shader.as_ref(),
        )
        .unwrap();
        delete_shader.forget();

        inner.into()
    }) as Box<dyn FnMut() -> JsValue>);
    Reflect::set(&persistent, &JsValue::from_str("open"), open.as_ref()).unwrap();
    open.forget();
    Reflect::set(
        &api,
        &JsValue::from_str("PersistentGpuCache"),
        persistent.as_ref(),
    )
    .unwrap();

    let _guard = PersistentCacheApiGuard::install(&api.into());

    let mut cache = ShaderCache::new();
    let dxbc = b"fake dxbc";
    let flags = make_flags();

    let (artifact, source) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
            panic!("persistent hit should not invoke translation");
        })
        .await
        .unwrap();

    assert_eq!(source, ShaderCacheSource::Persistent);
    assert_eq!(artifact.wgsl, "// wgsl persistent");
    assert_eq!(open_calls.get(), 1);
    assert_eq!(get_calls.get(), 1);
    assert_eq!(put_calls.get(), 0);

    let (_artifact2, source2) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
            panic!("second call should be served from in-memory cache");
        })
        .await
        .unwrap();
    assert_eq!(source2, ShaderCacheSource::Memory);
    assert_eq!(open_calls.get(), 1);
    assert_eq!(get_calls.get(), 1);
    assert_eq!(put_calls.get(), 0);
}

#[wasm_bindgen_test(async)]
async fn persistence_is_disabled_after_get_error() {
    let open_calls = Rc::new(Cell::new(0u32));
    let get_calls = Rc::new(Cell::new(0u32));
    let put_calls = Rc::new(Cell::new(0u32));
    let translate_calls = Rc::new(Cell::new(0u32));

    let api = Object::new();

    let compute =
        Closure::wrap(
            Box::new(move |_dxbc: JsValue, _flags: JsValue| JsValue::from_str("test-key"))
                as Box<dyn FnMut(JsValue, JsValue) -> JsValue>,
        );
    Reflect::set(
        &api,
        &JsValue::from_str("computeShaderCacheKey"),
        compute.as_ref(),
    )
    .unwrap();
    compute.forget();

    let persistent = Object::new();
    let open_calls_cb = open_calls.clone();
    let get_calls_cb = get_calls.clone();
    let put_calls_cb = put_calls.clone();

    let open = Closure::wrap(Box::new(move || {
        open_calls_cb.set(open_calls_cb.get() + 1);

        let inner = Object::new();

        let get_calls_inner = get_calls_cb.clone();
        let get_shader = Closure::wrap(Box::new(move |_key: JsValue| {
            get_calls_inner.set(get_calls_inner.get() + 1);
            js_sys::Promise::reject(&JsValue::from_str("get failed")).into()
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        Reflect::set(&inner, &JsValue::from_str("getShader"), get_shader.as_ref()).unwrap();
        get_shader.forget();

        let put_calls_inner = put_calls_cb.clone();
        let put_shader = Closure::wrap(Box::new(move |_key: JsValue, _value: JsValue| {
            put_calls_inner.set(put_calls_inner.get() + 1);
            JsValue::UNDEFINED
        }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
        Reflect::set(&inner, &JsValue::from_str("putShader"), put_shader.as_ref()).unwrap();
        put_shader.forget();

        let delete_shader =
            Closure::wrap(Box::new(move |_key: JsValue| JsValue::UNDEFINED)
                as Box<dyn FnMut(JsValue) -> JsValue>);
        Reflect::set(
            &inner,
            &JsValue::from_str("deleteShader"),
            delete_shader.as_ref(),
        )
        .unwrap();
        delete_shader.forget();

        inner.into()
    }) as Box<dyn FnMut() -> JsValue>);
    Reflect::set(&persistent, &JsValue::from_str("open"), open.as_ref()).unwrap();
    open.forget();
    Reflect::set(
        &api,
        &JsValue::from_str("PersistentGpuCache"),
        persistent.as_ref(),
    )
    .unwrap();

    let _guard = PersistentCacheApiGuard::install(&api.into());

    let mut cache = ShaderCache::new();
    let dxbc = b"fake dxbc";
    let flags = make_flags();

    let translate_calls_cb = translate_calls.clone();
    let (_artifact, source) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || {
            let translate_calls_cb = translate_calls_cb.clone();
            async move {
                translate_calls_cb.set(translate_calls_cb.get() + 1);
                Ok(make_artifact("translated"))
            }
        })
        .await
        .unwrap();
    assert_eq!(source, ShaderCacheSource::Translated);
    assert_eq!(open_calls.get(), 1);
    assert_eq!(get_calls.get(), 1);
    assert_eq!(put_calls.get(), 0);
    assert_eq!(translate_calls.get(), 1);

    let (_artifact2, source2) = cache
        .get_or_translate_with_source(dxbc, flags.clone(), move || async move {
            panic!("second call should be served from in-memory cache");
        })
        .await
        .unwrap();
    assert_eq!(source2, ShaderCacheSource::Memory);
    assert_eq!(open_calls.get(), 1);
    assert_eq!(get_calls.get(), 1);
    assert_eq!(put_calls.get(), 0);
    assert_eq!(translate_calls.get(), 1);
}
