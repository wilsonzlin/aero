//! Experimental AeroGPU D3D9 execution helpers.
//!
//! The Win7 D3D9 UMD writes shader blobs into the `CREATE_SHADER_DXBC` protocol payload, but those
//! blobs are not guaranteed to be DXBC containers. D3D9 commonly submits the legacy SM2/SM3 DWORD
//! token stream directly (e.g. `vs_2_0`, `ps_3_0`).
//!
//! The host-side executor must therefore accept both:
//! - DXBC containers (blob starts with ASCII `DXBC`)
//! - raw D3D9 token streams (no `DXBC` header)
//!
//! This module provides:
//! - payload format detection + DXBC extraction
//! - translation to WGSL using `aero-d3d9`
//! - caching of WGSL + compiled `wgpu::ShaderModule`s keyed by a strong hash of the original bytes

use std::collections::HashMap;

use aero_d3d9::sm3::wgsl::{BindGroupLayout, Sm3WgslError};
use aero_d3d9::sm3::{ShaderStage, ShaderVersion};
use aero_dxbc::{DxbcError, DxbcFile};
use tracing::debug;

/// Maximum accepted D3D9 shader payload size in bytes.
///
/// D3D9 shader blobs are treated as untrusted input. Bounding the maximum size avoids excessive
/// hashing/parsing work and prevents pathological payloads from triggering large host allocations.
///
/// Note: This is intentionally generous compared to real-world SM2/SM3 shaders and is primarily
/// meant as a guardrail for the experimental shader cache.
pub const MAX_D3D9_SHADER_BLOB_BYTES: usize = 512 * 1024; // 512 KiB

/// On-the-wire shader blob format carried by `CREATE_SHADER_DXBC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderPayloadFormat {
    /// A DXBC container (starts with `DXBC` magic).
    Dxbc,
    /// A raw D3D9 DWORD token stream (starts with a version token like `0xFFFE0200`).
    D3d9TokenStream,
}

impl ShaderPayloadFormat {
    pub fn detect(bytes: &[u8]) -> Self {
        if bytes.starts_with(b"DXBC") {
            Self::Dxbc
        } else {
            Self::D3d9TokenStream
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum D3d9ShaderCacheError {
    #[error("DXBC container missing SHDR/SHEX shader chunk")]
    DxbcMissingShaderChunk,
    #[error("shader payload length {len} exceeds maximum {max} bytes")]
    PayloadTooLarge { len: usize, max: usize },
    #[error("dxbc error: {0}")]
    Dxbc(#[from] DxbcError),
    #[error("sm3 translation error: {0}")]
    Sm3(#[from] Sm3WgslError),
    #[error("shader handle {0} already exists")]
    HandleAlreadyExists(u32),
    #[error("shader stage mismatch: expected {expected:?}, found {found:?}")]
    StageMismatch {
        expected: ShaderStage,
        found: ShaderStage,
    },
}

#[derive(Debug)]
pub struct CachedD3d9Shader {
    pub hash: blake3::Hash,
    pub payload_format: ShaderPayloadFormat,
    pub version: ShaderVersion,
    pub wgsl: String,
    pub entry_point: &'static str,
    pub bind_group_layout: BindGroupLayout,
    pub module: wgpu::ShaderModule,
}

/// Cache for translated D3D9 shaders.
///
/// The cache is keyed by a BLAKE3 hash of the original bytes submitted by the guest (which may be
/// either DXBC or a raw token stream). This avoids repeated DXBC parsing + WGSL emission and also
/// avoids recompiling the resulting WGSL into `wgpu::ShaderModule`s on repeated binds.
#[derive(Default)]
pub struct D3d9ShaderCache {
    /// hash(original_bytes) -> translated+compiled artifact
    by_hash: HashMap<blake3::Hash, CachedD3d9Shader>,
    /// protocol handle -> hash(original_bytes)
    by_handle: HashMap<u32, blake3::Hash>,
}

impl D3d9ShaderCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.by_hash.clear();
        self.by_handle.clear();
    }

    pub fn get(&self, handle: u32) -> Option<&CachedD3d9Shader> {
        let hash = self.by_handle.get(&handle)?;
        self.by_hash.get(hash)
    }

    pub fn create_shader(
        &mut self,
        device: &wgpu::Device,
        handle: u32,
        expected_stage: ShaderStage,
        bytes: &[u8],
    ) -> Result<(), D3d9ShaderCacheError> {
        if self.by_handle.contains_key(&handle) {
            return Err(D3d9ShaderCacheError::HandleAlreadyExists(handle));
        }
        if bytes.len() > MAX_D3D9_SHADER_BLOB_BYTES {
            return Err(D3d9ShaderCacheError::PayloadTooLarge {
                len: bytes.len(),
                max: MAX_D3D9_SHADER_BLOB_BYTES,
            });
        }

        let hash = blake3::hash(bytes);
        let payload_format = ShaderPayloadFormat::detect(bytes);

        // Ensure the shader artifact exists (translate/compile on miss).
        let hash = match self.by_hash.entry(hash) {
            std::collections::hash_map::Entry::Occupied(entry) => *entry.key(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let token_stream = extract_token_stream(payload_format, bytes)?;
                let translated = aero_d3d9::sm3::wgsl::translate_to_wgsl(token_stream)?;

                let hash = *entry.key();
                debug!(
                    shader_hash = %hash.to_hex(),
                    format = ?payload_format,
                    stage = ?translated.version.stage,
                    sm_major = translated.version.major,
                    sm_minor = translated.version.minor,
                    "aerogpu d3d9 shader payload"
                );

                let label = format!(
                    "aerogpu-d3d9-shader-{:?}-{}",
                    translated.version.stage,
                    hash.to_hex()
                );
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(&label),
                    source: wgpu::ShaderSource::Wgsl(translated.wgsl.clone().into()),
                });

                entry.insert(CachedD3d9Shader {
                    hash,
                    payload_format,
                    version: translated.version,
                    wgsl: translated.wgsl,
                    entry_point: translated.entry_point,
                    bind_group_layout: translated.bind_group_layout,
                    module,
                });
                hash
            }
        };

        let artifact = self
            .by_hash
            .get(&hash)
            .expect("shader artifact missing after insertion");
        if artifact.version.stage != expected_stage {
            return Err(D3d9ShaderCacheError::StageMismatch {
                expected: expected_stage,
                found: artifact.version.stage,
            });
        }
        self.by_handle.insert(handle, hash);
        Ok(())
    }

    pub fn destroy_shader(&mut self, handle: u32) {
        self.by_handle.remove(&handle);
    }
}

fn extract_token_stream(
    format: ShaderPayloadFormat,
    bytes: &[u8],
) -> Result<&[u8], D3d9ShaderCacheError> {
    match format {
        ShaderPayloadFormat::D3d9TokenStream => Ok(bytes),
        ShaderPayloadFormat::Dxbc => {
            let dxbc = DxbcFile::parse(bytes)?;
            dxbc.find_first_shader_chunk()
                .map(|chunk| chunk.data)
                .ok_or(D3d9ShaderCacheError::DxbcMissingShaderChunk)
        }
    }
}
