//! D3D9 shader cache with persistent backing store.
//!
//! In Aero's architecture, the DXBC -> WGSL translation runs in Rust (WASM) and
//! the resulting WGSL is compiled by the browser WebGPU implementation.
//! DXBC parsing + translation can take multiple seconds for large shader sets;
//! persisting the translation output avoids repeating this work on subsequent
//! boots/runs.
//!
//! This module is written to be "drop-in" in a real `aero-d3d9` crate. The repo
//! this agent operates on only includes documentation, so the surrounding
//! runtime types (GPU device handles, logging, etc) are intentionally abstracted.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// Translation flags that affect WGSL output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShaderTranslationFlags {
    pub half_pixel_center: bool,
    /// Stable hash representing relevant WebGPU capabilities/limits/features.
    pub caps_hash: String,
}

/// Persisted output of DXBC -> WGSL translation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedShaderArtifact {
    pub wgsl: String,
    /// Reflection/binding layout metadata (bind groups, bindings, etc).
    pub reflection: serde_json::Value,
}

/// Strong key for shader translation artifacts.
///
/// The key is derived from:
/// - raw DXBC bytecode
/// - translation flags (half-pixel mode, caps hash)
/// - a version constant baked into the JS persistent cache key derivation
///
/// The JS side is responsible for the canonical key derivation and versioning.
/// Rust computes the same key to look up cached artifacts without translating.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShaderCacheKey(pub String);

#[wasm_bindgen(module = "/web/gpu-cache/persistent_cache.ts")]
extern "C" {
    #[wasm_bindgen(js_name = computeShaderCacheKey)]
    async fn js_compute_shader_cache_key(dxbc: js_sys::Uint8Array, flags: JsValue) -> JsValue;

    #[wasm_bindgen(js_name = PersistentGpuCache)]
    type JsPersistentGpuCache;

    #[wasm_bindgen(static_method_of = JsPersistentGpuCache, js_name = open)]
    async fn js_open_persistent_cache() -> JsValue;

    #[wasm_bindgen(method, js_name = getShader)]
    async fn js_persistent_get_shader(this: &JsPersistentGpuCache, key: String) -> JsValue;

    #[wasm_bindgen(method, js_name = putShader)]
    async fn js_persistent_put_shader(this: &JsPersistentGpuCache, key: String, value: JsValue);

    #[wasm_bindgen(method, js_name = deleteShader)]
    async fn js_persistent_delete_shader(this: &JsPersistentGpuCache, key: String);
}

async fn compute_key(dxbc: &[u8], flags: &ShaderTranslationFlags) -> Result<ShaderCacheKey, JsValue> {
    let dxbc_u8 = js_sys::Uint8Array::from(dxbc);
    let flags_js = serde_wasm_bindgen::to_value(flags).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let key_js = js_compute_shader_cache_key(dxbc_u8, flags_js).await;
    Ok(ShaderCacheKey(key_js.as_string().ok_or_else(|| JsValue::from_str("computeShaderCacheKey did not return a string"))?))
}

async fn open_persistent_cache() -> Result<JsPersistentGpuCache, JsValue> {
    let cache_val = js_open_persistent_cache().await;
    cache_val.dyn_into::<JsPersistentGpuCache>()
}

/// In-memory cache for the current session, backed by a persistent store.
pub struct ShaderCache {
    in_memory: HashMap<ShaderCacheKey, PersistedShaderArtifact>,
}

impl ShaderCache {
    pub fn new() -> Self {
        Self {
            in_memory: HashMap::new(),
        }
    }

    /// Look up a shader translation artifact.
    ///
    /// On cache hit:
    /// - return the cached WGSL + reflection, skipping translation
    /// - the JS side will still compile WGSL to a GPUShaderModule
    ///
    /// On cache miss:
    /// - call `translate_fn` to perform DXBC -> WGSL
    /// - persist the result via IndexedDB/OPFS
    ///
    /// If the browser fails to compile a cached WGSL module, the JS side should
    /// delete the persistent entry and request retranslation.
    pub async fn get_or_translate<F, Fut>(
        &mut self,
        dxbc: &[u8],
        flags: ShaderTranslationFlags,
        translate_fn: F,
    ) -> Result<PersistedShaderArtifact, JsValue>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = PersistedShaderArtifact>,
    {
        let key = compute_key(dxbc, &flags).await?;

        if let Some(hit) = self.in_memory.get(&key) {
            return Ok(hit.clone());
        }

        let persistent = open_persistent_cache().await?;
        let cached_val = js_persistent_get_shader(&persistent, key.0.clone()).await;
        if !cached_val.is_undefined() && !cached_val.is_null() {
            let cached: PersistedShaderArtifact =
                serde_wasm_bindgen::from_value(cached_val).map_err(|e| JsValue::from_str(&e.to_string()))?;
            self.in_memory.insert(key, cached.clone());
            return Ok(cached);
        }

        // Cache miss: translate and persist.
        let translated = translate_fn().await;
        let translated_js = serde_wasm_bindgen::to_value(&translated).map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_persistent_put_shader(&persistent, key.0.clone(), translated_js).await;

        self.in_memory.insert(key, translated.clone());
        Ok(translated)
    }

    /// Remove a shader entry from both in-memory and persistent caches.
    pub async fn invalidate(&mut self, dxbc: &[u8], flags: ShaderTranslationFlags) -> Result<(), JsValue> {
        let key = compute_key(dxbc, &flags).await?;
        self.in_memory.remove(&key);

        let persistent = open_persistent_cache().await?;
        js_persistent_delete_shader(&persistent, key.0).await;
        Ok(())
    }
}
