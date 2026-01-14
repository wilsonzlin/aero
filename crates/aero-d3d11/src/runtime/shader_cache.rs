//! D3D11 shader cache with persistent backing store.
//!
//! This module implements a best-effort persistent cache for DXBC -> WGSL translation
//! artifacts. Large shader sets can take seconds to decode+translate; persisting
//! WGSL + minimal reflection metadata allows subsequent browser sessions to skip
//! the translation step entirely.
//!
//! The persistent backing store is implemented in JS (`web/gpu-cache/persistent_cache.ts`)
//! using IndexedDB + OPFS with an LRU-ish eviction policy (max entries/bytes). This
//! Rust module is a thin wasm-only wrapper around that implementation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use super::bindings::ShaderStage;

/// Version used to invalidate persisted D3D11 DXBC -> WGSL translation artifacts.
///
/// Bump this when the Rust translator's output *semantics* change in a way that could still
/// compile successfully but behave differently.
///
/// Note: This is separate from the JS-side `CACHE_SCHEMA_VERSION` because it is easy to forget
/// to bump that global cache version when only the D3D11 translator changes.
pub const D3D11_TRANSLATOR_CACHE_VERSION: u32 = 1;

fn default_d3d11_translator_cache_version() -> u32 {
    D3D11_TRANSLATOR_CACHE_VERSION
}

/// Translation flags that affect WGSL output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShaderTranslationFlags {
    /// D3D11 translator version that participates in cache key derivation.
    ///
    /// Always set this to [`D3D11_TRANSLATOR_CACHE_VERSION`] for persistent cache keys so any
    /// semantic translation changes safely invalidate cached WGSL.
    #[serde(default = "default_d3d11_translator_cache_version")]
    pub d3d11_translator_version: u32,
    /// Stable hash representing relevant WebGPU capabilities/limits/features.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps_hash: Option<String>,
}

impl ShaderTranslationFlags {
    pub fn new(caps_hash: Option<String>) -> Self {
        Self {
            d3d11_translator_version: D3D11_TRANSLATOR_CACHE_VERSION,
            caps_hash,
        }
    }
}

impl Default for ShaderTranslationFlags {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Persisted shader stage for cached artifacts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PersistedShaderStage {
    Vertex,
    Pixel,
    Compute,
    /// Shader stages that the AeroGPU WebGPU pipeline cannot execute (e.g. GS/HS/DS).
    ///
    /// These are accepted-but-ignored by the command executor for robustness. Persisting the
    /// "ignored" result avoids repeatedly parsing the same unsupported shaders.
    Ignored,
}

impl PersistedShaderStage {
    pub fn from_stage(stage: ShaderStage) -> Self {
        match stage {
            ShaderStage::Vertex => Self::Vertex,
            ShaderStage::Pixel => Self::Pixel,
            ShaderStage::Compute => Self::Compute,
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => Self::Ignored,
        }
    }

    pub fn to_stage(self) -> Option<ShaderStage> {
        match self {
            Self::Vertex => Some(ShaderStage::Vertex),
            Self::Pixel => Some(ShaderStage::Pixel),
            Self::Compute => Some(ShaderStage::Compute),
            Self::Ignored => None,
        }
    }
}

/// Serializable subset of `crate::BindingKind`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PersistedBindingKind {
    ConstantBuffer { slot: u32, reg_count: u32 },
    Texture2D { slot: u32 },
    SrvBuffer { slot: u32 },
    Sampler { slot: u32 },
    UavBuffer { slot: u32 },
}

impl PersistedBindingKind {
    pub fn from_kind(kind: &crate::BindingKind) -> Self {
        match kind {
            crate::BindingKind::ConstantBuffer { slot, reg_count } => {
                Self::ConstantBuffer {
                    slot: *slot,
                    reg_count: *reg_count,
                }
            }
            crate::BindingKind::Texture2D { slot } => Self::Texture2D { slot: *slot },
            crate::BindingKind::SrvBuffer { slot } => Self::SrvBuffer { slot: *slot },
            crate::BindingKind::Sampler { slot } => Self::Sampler { slot: *slot },
            crate::BindingKind::UavBuffer { slot } => Self::UavBuffer { slot: *slot },
        }
    }

    pub fn to_kind(&self) -> crate::BindingKind {
        match self {
            Self::ConstantBuffer { slot, reg_count } => crate::BindingKind::ConstantBuffer {
                slot: *slot,
                reg_count: *reg_count,
            },
            Self::Texture2D { slot } => crate::BindingKind::Texture2D { slot: *slot },
            Self::SrvBuffer { slot } => crate::BindingKind::SrvBuffer { slot: *slot },
            Self::Sampler { slot } => crate::BindingKind::Sampler { slot: *slot },
            Self::UavBuffer { slot } => crate::BindingKind::UavBuffer { slot: *slot },
        }
    }
}

/// Serializable subset of `crate::Binding`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedBinding {
    pub group: u32,
    pub binding: u32,
    /// `wgpu::ShaderStages` bitmask.
    pub visibility_bits: u32,
    pub kind: PersistedBindingKind,
}

impl PersistedBinding {
    pub fn from_binding(binding: &crate::Binding) -> Self {
        Self {
            group: binding.group,
            binding: binding.binding,
            visibility_bits: binding.visibility.bits(),
            kind: PersistedBindingKind::from_kind(&binding.kind),
        }
    }

    pub fn to_binding(&self) -> crate::Binding {
        crate::Binding {
            group: self.group,
            binding: self.binding,
            visibility: wgpu::ShaderStages::from_bits_truncate(self.visibility_bits),
            kind: self.kind.to_kind(),
        }
    }
}

/// Serializable `VsInputSignatureElement` for cached vertex shaders.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedVsInputSignatureElement {
    pub semantic_name_hash: u32,
    pub semantic_index: u32,
    pub input_register: u32,
    pub mask: u8,
    pub shader_location: u32,
}

impl PersistedVsInputSignatureElement {
    pub fn from_element(e: &crate::input_layout::VsInputSignatureElement) -> Self {
        Self {
            semantic_name_hash: e.semantic_name_hash,
            semantic_index: e.semantic_index,
            input_register: e.input_register,
            mask: e.mask,
            shader_location: e.shader_location,
        }
    }

    pub fn to_element(self) -> crate::input_layout::VsInputSignatureElement {
        crate::input_layout::VsInputSignatureElement {
            semantic_name_hash: self.semantic_name_hash,
            semantic_index: self.semantic_index,
            input_register: self.input_register,
            mask: self.mask,
            shader_location: self.shader_location,
        }
    }
}

/// Persisted output of DXBC -> WGSL translation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedShaderArtifact {
    pub wgsl: String,
    pub stage: PersistedShaderStage,
    /// Bind-group reflection metadata used for pipeline layout construction.
    #[serde(default)]
    pub bindings: Vec<PersistedBinding>,
    /// Vertex shader input signature derived from DXBC `ISGN`, when available.
    #[serde(default)]
    pub vs_input_signature: Vec<PersistedVsInputSignatureElement>,
}

/// Strong key for shader translation artifacts.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct InMemoryShaderCacheKey([u8; 32]);

fn compute_in_memory_key(dxbc: &[u8], flags: &ShaderTranslationFlags) -> InMemoryShaderCacheKey {
    // Keep this independent from the JS persistent key derivation so in-memory caching
    // still functions even when the host didn't install the persistent cache API.
    const VERSION: &[u8] = b"aero-d3d11 in-memory shader cache v1";

    let mut hasher = blake3::Hasher::new();
    hasher.update(VERSION);
    hasher.update(dxbc);
    hasher.update(&flags.d3d11_translator_version.to_le_bytes());
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShaderCacheStats {
    pub translate_calls: u64,
    pub persistent_hits: u64,
    pub persistent_misses: u64,
    pub memory_hits: u64,
    pub persistent_disabled: bool,
}

/// In-memory cache for the current session, backed by a persistent store.
pub struct ShaderCache {
    in_memory: HashMap<InMemoryShaderCacheKey, PersistedShaderArtifact>,
    persistent_state: PersistentCacheState,
    persistent: Option<JsPersistentGpuCache>,

    translate_calls: u64,
    persistent_hits: u64,
    persistent_misses: u64,
    memory_hits: u64,
}

impl ShaderCache {
    pub fn new() -> Self {
        Self {
            in_memory: HashMap::new(),
            persistent_state: PersistentCacheState::Uninitialized,
            persistent: None,
            translate_calls: 0,
            persistent_hits: 0,
            persistent_misses: 0,
            memory_hits: 0,
        }
    }

    pub fn stats(&self) -> ShaderCacheStats {
        ShaderCacheStats {
            translate_calls: self.translate_calls,
            persistent_hits: self.persistent_hits,
            persistent_misses: self.persistent_misses,
            memory_hits: self.memory_hits,
            persistent_disabled: self.is_persistent_disabled(),
        }
    }

    /// Returns `true` if the browser persistent shader cache (IndexedDB/OPFS) has been disabled
    /// for the remainder of the session.
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
        let mem_key = compute_in_memory_key(dxbc, &flags);

        if let Some(hit) = self.in_memory.get(&mem_key) {
            self.memory_hits = self.memory_hits.saturating_add(1);
            return Ok((hit.clone(), ShaderCacheSource::Memory));
        }

        // Best-effort persistent lookup.
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
                                    self.persistent_hits = self.persistent_hits.saturating_add(1);
                                    self.in_memory.insert(mem_key, cached.clone());
                                    return Ok((cached, ShaderCacheSource::Persistent));
                                }
                                Err(_err) => {
                                    // Cached entry is unreadable/corrupt/out-of-date; best-effort
                                    // delete and retranslate.
                                    if let Err(_err) = persistent
                                        .js_persistent_delete_shader(persistent_key.0.clone())
                                        .await
                                    {
                                        self.disable_persistent();
                                    }
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
        self.translate_calls = self.translate_calls.saturating_add(1);

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

        // Only count this as a *persistent* cache miss when persistence is actually available.
        if !self.is_persistent_disabled() {
            self.persistent_misses = self.persistent_misses.saturating_add(1);
        }

        Ok((translated, ShaderCacheSource::Translated))
    }

    /// Remove a shader entry from both in-memory and persistent caches.
    pub async fn invalidate(
        &mut self,
        dxbc: &[u8],
        flags: ShaderTranslationFlags,
    ) -> Result<(), JsValue> {
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
