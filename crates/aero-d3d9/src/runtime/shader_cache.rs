//! D3D9 shader cache with persistent backing store.
//!
//! In Aero's architecture, the DXBC -> WGSL translation runs in Rust (WASM) and
//! the resulting WGSL is compiled by the browser WebGPU implementation.
//! DXBC parsing + translation can take multiple seconds for large shader sets;
//! persisting the translation output avoids repeating this work on subsequent
//! boots/runs.
//!
//! This module is a thin Rust wrapper around the browser-side persistent cache
//! implementation (`web/gpu-cache/persistent_cache.ts`). It is only built for
//! `wasm32` targets and is wired into the D3D9 executor (`crates/aero-gpu`) so
//! DXBC -> WGSL translation output can be persisted across runs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::shader_limits::MAX_D3D9_SHADER_BLOB_BYTES;

/// Version used to invalidate persisted D3D9 DXBC -> WGSL translation artifacts.
///
/// Bump this when the Rust translator's output *semantics* change in a way that could still
/// compile successfully but behave differently (i.e. a correctness fix).
///
/// Note: This is separate from the JS-side `CACHE_SCHEMA_VERSION` because it is easy to forget
/// to bump that global cache version when only the D3D9 translator changes.
// v4: shader translation now optionally applies the D3D9 half-pixel center convention when
// `ShaderTranslationFlags::half_pixel_center` is enabled.
// v5: add `lrp` opcode support in the D3D9 translators (affects WGSL semantics for shaders that
// previously translated but produced incorrect output).
// v6: adaptive vertex input semantic→location mapping (incl. TEXCOORD8+).
// v7: fix SM3 semantic location reflection for declared-but-unused vertex inputs (ensures
// executor-side vertex declaration binding stays collision-free).
// v8: SM3 WGSL generator now supports the half-pixel-center option (emits the `half_pixel` uniform
// and clip-space adjustment in vertex shaders).
// v9: lower `dcl_1d` samplers to `texture_2d` bindings (height=1) so the D3D9 executor can bind
// them as ordinary 2D textures.
pub const D3D9_TRANSLATOR_CACHE_VERSION: u32 = 9;

fn default_d3d9_translator_cache_version() -> u32 {
    D3D9_TRANSLATOR_CACHE_VERSION
}

/// Translation flags that affect WGSL output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShaderTranslationFlags {
    /// D3D9 translator version that participates in cache key derivation.
    ///
    /// Always set this to [`D3D9_TRANSLATOR_CACHE_VERSION`] for persistent cache keys so any
    /// semantic translation changes safely invalidate cached WGSL.
    #[serde(default = "default_d3d9_translator_cache_version")]
    pub d3d9_translator_version: u32,
    pub half_pixel_center: bool,
    /// Stable hash representing relevant WebGPU capabilities/limits/features.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps_hash: Option<String>,
}

impl ShaderTranslationFlags {
    pub fn new(half_pixel_center: bool, caps_hash: Option<String>) -> Self {
        Self {
            d3d9_translator_version: D3D9_TRANSLATOR_CACHE_VERSION,
            half_pixel_center,
            caps_hash,
        }
    }
}

impl Default for ShaderTranslationFlags {
    fn default() -> Self {
        Self::new(false, None)
    }
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
/// - translation flags (half-pixel mode, caps hash, translator version)
/// - a version constant baked into the JS persistent cache key derivation (`CACHE_SCHEMA_VERSION`)
///
/// The JS side is responsible for the canonical key derivation and versioning.
/// Rust computes the same key to look up cached artifacts without translating.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShaderCacheKey(pub String);

#[wasm_bindgen(module = "/js/persistent_cache_shim.js")]
extern "C" {
    #[wasm_bindgen(js_name = computeShaderCacheKey, catch)]
    async fn js_compute_shader_cache_key(
        dxbc: js_sys::Uint8Array,
        flags: JsValue,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = PersistentGpuCache)]
    #[derive(Clone)]
    type JsPersistentGpuCache;

    #[wasm_bindgen(static_method_of = JsPersistentGpuCache, js_name = open, catch)]
    async fn js_open_persistent_cache() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, js_name = getShader, catch)]
    async fn js_persistent_get_shader(
        this: &JsPersistentGpuCache,
        key: String,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, js_name = putShader, catch)]
    async fn js_persistent_put_shader(
        this: &JsPersistentGpuCache,
        key: String,
        value: JsValue,
    ) -> Result<(), JsValue>;

    #[wasm_bindgen(method, js_name = deleteShader, catch)]
    async fn js_persistent_delete_shader(
        this: &JsPersistentGpuCache,
        key: String,
    ) -> Result<(), JsValue>;
}

fn compute_in_memory_key(dxbc: &[u8], flags: &ShaderTranslationFlags) -> InMemoryShaderCacheKey {
    // Keep this independent from the JS persistent key derivation so in-memory caching
    // still functions even when the host didn't install the persistent cache API.
    const VERSION: &[u8] = b"aero-d3d9 in-memory shader cache v1";

    let mut hasher = blake3::Hasher::new();
    hasher.update(VERSION);
    hasher.update(dxbc);
    hasher.update(&flags.d3d9_translator_version.to_le_bytes());
    hasher.update(&[flags.half_pixel_center as u8]);
    match &flags.caps_hash {
        Some(caps_hash) => {
            hasher.update(&[1]);
            hasher.update(&(caps_hash.len() as u32).to_le_bytes());
            hasher.update(caps_hash.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    InMemoryShaderCacheKey(*hasher.finalize().as_bytes())
}

async fn compute_persistent_key(
    dxbc: &[u8],
    flags: &ShaderTranslationFlags,
) -> Result<ShaderCacheKey, JsValue> {
    if dxbc.len() > MAX_D3D9_SHADER_BLOB_BYTES {
        return Err(JsValue::from_str(&format!(
            "shader bytecode length {} exceeds maximum {} bytes",
            dxbc.len(),
            MAX_D3D9_SHADER_BLOB_BYTES
        )));
    }
    let dxbc_u8 = js_sys::Uint8Array::from(dxbc);
    let flags_js =
        serde_wasm_bindgen::to_value(flags).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let key_js = js_compute_shader_cache_key(dxbc_u8, flags_js).await?;
    Ok(ShaderCacheKey(key_js.as_string().ok_or_else(|| {
        JsValue::from_str("computeShaderCacheKey did not return a string")
    })?))
}

async fn open_persistent_cache() -> Result<JsPersistentGpuCache, JsValue> {
    let cache_val = JsPersistentGpuCache::js_open_persistent_cache().await?;
    cache_val.dyn_into::<JsPersistentGpuCache>()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct InMemoryShaderCacheKey([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistentCacheState {
    Uninitialized,
    Ready,
    Disabled,
}

/// Where a shader translation artifact was sourced from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderCacheSource {
    /// The artifact was found in the per-session in-memory cache.
    Memory,
    /// The artifact was loaded from the browser persistent cache (IndexedDB/OPFS) and
    /// inserted into the in-memory cache for faster subsequent lookups.
    Persistent,
    /// The artifact was produced by running shader translation (cache miss).
    Translated,
}

/// In-memory cache for the current session, backed by a persistent store.
pub struct ShaderCache {
    in_memory: HashMap<InMemoryShaderCacheKey, PersistedShaderArtifact>,
    persistent_state: PersistentCacheState,
    persistent: Option<JsPersistentGpuCache>,
}

impl Default for ShaderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ShaderCache {
    pub fn new() -> Self {
        Self {
            in_memory: HashMap::new(),
            persistent_state: PersistentCacheState::Uninitialized,
            persistent: None,
        }
    }

    /// Returns `true` if the browser persistent shader cache (IndexedDB/OPFS) has been disabled
    /// for the remainder of the session.
    ///
    /// Persistence is treated as best-effort: any failure to open the backing store, compute a
    /// persistent key, or read/write entries will disable persistence and the cache will fall back
    /// to in-memory-only behavior.
    pub fn is_persistent_disabled(&self) -> bool {
        self.persistent_state == PersistentCacheState::Disabled
    }

    fn disable_persistent(&mut self) {
        self.persistent_state = PersistentCacheState::Disabled;
        self.persistent = None;
    }

    async fn get_persistent_cache(&mut self) -> Option<JsPersistentGpuCache> {
        match self.persistent_state {
            PersistentCacheState::Disabled => None,
            PersistentCacheState::Ready => self.persistent.clone(),
            PersistentCacheState::Uninitialized => match open_persistent_cache().await {
                Ok(cache) => {
                    self.persistent_state = PersistentCacheState::Ready;
                    self.persistent = Some(cache.clone());
                    Some(cache)
                }
                Err(_err) => {
                    self.disable_persistent();
                    None
                }
            },
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
        let (artifact, _source) =
            self.get_or_translate_with_source(dxbc, flags, move || async move {
                Ok(translate_fn().await)
            })
            .await?;
        Ok(artifact)
    }

    /// Like [`ShaderCache::get_or_translate`], but also indicates whether the artifact came
    /// from the in-memory cache, persistent cache, or translation.
    ///
    /// Any error returned from this method indicates a *translation* failure; failures to
    /// open/read/write the persistent cache are treated as best-effort and will disable
    /// persistence for the remainder of the session.
    pub async fn get_or_translate_with_source<F, Fut>(
        &mut self,
        dxbc: &[u8],
        flags: ShaderTranslationFlags,
        translate_fn: F,
    ) -> Result<(PersistedShaderArtifact, ShaderCacheSource), JsValue>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<PersistedShaderArtifact, String>>,
    {
        if dxbc.len() > MAX_D3D9_SHADER_BLOB_BYTES {
            return Err(JsValue::from_str(&format!(
                "shader bytecode length {} exceeds maximum {} bytes",
                dxbc.len(),
                MAX_D3D9_SHADER_BLOB_BYTES
            )));
        }
        let mem_key = compute_in_memory_key(dxbc, &flags);

        if let Some(hit) = self.in_memory.get(&mem_key) {
            return Ok((hit.clone(), ShaderCacheSource::Memory));
        }

        // Best-effort persistent lookup:
        // - Errors interacting with the persistent backing store (missing APIs, permission errors,
        //   quota issues, etc.) disable persistence for the remainder of the session.
        // - Corrupt/unreadable entries are treated as cache misses (with a best-effort delete),
        //   but do not disable persistence so a fresh translation can repair the entry.
        let mut persistent_key: Option<ShaderCacheKey> = None;
        if self.persistent_state != PersistentCacheState::Disabled {
            match compute_persistent_key(dxbc, &flags).await {
                Ok(key) => persistent_key = Some(key),
                Err(_err) => self.disable_persistent(),
            }
        }

        if let Some(persistent_key) = persistent_key.clone() {
            if let Some(persistent) = self.get_persistent_cache().await {
                match persistent
                    .js_persistent_get_shader(persistent_key.0.clone())
                    .await
                {
                    Ok(cached_val) => {
                        if !cached_val.is_undefined() && !cached_val.is_null() {
                            match serde_wasm_bindgen::from_value::<PersistedShaderArtifact>(
                                cached_val,
                            ) {
                                Ok(cached) => {
                                    self.in_memory.insert(mem_key, cached.clone());
                                    return Ok((cached, ShaderCacheSource::Persistent));
                                }
                                Err(_err) => {
                                    // Cached entry is unreadable/corrupt/out-of-date; best-effort
                                    // delete and retranslate.
                                    let _ = persistent
                                        .js_persistent_delete_shader(persistent_key.0.clone())
                                        .await;
                                }
                            }
                        }
                    }
                    Err(_err) => {
                        self.disable_persistent();
                    }
                }
            }
        }

        // Cache miss: translate.
        let translated = translate_fn().await.map_err(|e| JsValue::from_str(&e))?;

        // Populate the per-session cache regardless of persistent availability.
        self.in_memory.insert(mem_key, translated.clone());

        // Best-effort persistent write.
        if self.persistent_state != PersistentCacheState::Disabled {
            let persistent_key = match persistent_key {
                Some(key) => Some(key),
                None => match compute_persistent_key(dxbc, &flags).await {
                    Ok(key) => Some(key),
                    Err(_err) => {
                        self.disable_persistent();
                        None
                    }
                },
            };

            if let (Some(persistent_key), Some(persistent)) =
                (persistent_key, self.get_persistent_cache().await)
            {
                match serde_wasm_bindgen::to_value(&translated) {
                    Ok(translated_js) => {
                        if let Err(_err) = persistent
                            .js_persistent_put_shader(persistent_key.0.clone(), translated_js)
                            .await
                        {
                            self.disable_persistent();
                        }
                    }
                    Err(_err) => {
                        self.disable_persistent();
                    }
                }
            }
        }

        Ok((translated, ShaderCacheSource::Translated))
    }

    /// Remove a shader entry from both in-memory and persistent caches.
    pub async fn invalidate(
        &mut self,
        dxbc: &[u8],
        flags: ShaderTranslationFlags,
    ) -> Result<(), JsValue> {
        if dxbc.len() > MAX_D3D9_SHADER_BLOB_BYTES {
            return Err(JsValue::from_str(&format!(
                "shader bytecode length {} exceeds maximum {} bytes",
                dxbc.len(),
                MAX_D3D9_SHADER_BLOB_BYTES
            )));
        }
        let mem_key = compute_in_memory_key(dxbc, &flags);
        self.in_memory.remove(&mem_key);

        if self.persistent_state != PersistentCacheState::Disabled {
            let persistent_key = match compute_persistent_key(dxbc, &flags).await {
                Ok(key) => Some(key),
                Err(_err) => {
                    self.disable_persistent();
                    None
                }
            };

            if let (Some(persistent_key), Some(persistent)) =
                (persistent_key, self.get_persistent_cache().await)
            {
                if let Err(_err) = persistent
                    .js_persistent_delete_shader(persistent_key.0.clone())
                    .await
                {
                    self.disable_persistent();
                }
            }
        }
        Ok(())
    }
}
