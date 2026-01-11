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

use aero_d3d9::dxbc::{Container as DxbcContainer, DxbcError, FourCC};
use aero_d3d9::shader::{self, ShaderError, ShaderStage, ShaderVersion};
use tracing::debug;

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
        if bytes.len() >= 4 && &bytes[0..4] == b"DXBC" {
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
    #[error("dxbc error: {0}")]
    Dxbc(#[from] DxbcError),
    #[error("shader translation error: {0}")]
    Shader(#[from] ShaderError),
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
    pub bind_group_layout: shader::BindGroupLayout,
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

        let hash = blake3::hash(bytes);
        let payload_format = ShaderPayloadFormat::detect(bytes);

        // Ensure the shader artifact exists (translate/compile on miss).
        if !self.by_hash.contains_key(&hash) {
            let token_stream = extract_token_stream(payload_format, bytes)?;
            let program = shader::parse(token_stream)?;

            debug!(
                shader_hash = %hash.to_hex(),
                format = ?payload_format,
                stage = ?program.version.stage,
                sm_major = program.version.model.major,
                sm_minor = program.version.model.minor,
                "aerogpu d3d9 shader payload"
            );

            let ir = shader::to_ir(&program);
            let wgsl = shader::generate_wgsl(&ir);

            let label = format!(
                "aerogpu-d3d9-shader-{:?}-{}",
                program.version.stage,
                hash.to_hex()
            );
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&label),
                source: wgpu::ShaderSource::Wgsl(wgsl.wgsl.clone().into()),
            });

            self.by_hash.insert(
                hash,
                CachedD3d9Shader {
                    hash,
                    payload_format,
                    version: program.version,
                    wgsl: wgsl.wgsl,
                    entry_point: wgsl.entry_point,
                    bind_group_layout: wgsl.bind_group_layout,
                    module,
                },
            );
        }

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

fn extract_token_stream<'a>(
    format: ShaderPayloadFormat,
    bytes: &'a [u8],
) -> Result<&'a [u8], D3d9ShaderCacheError> {
    match format {
        ShaderPayloadFormat::D3d9TokenStream => Ok(bytes),
        ShaderPayloadFormat::Dxbc => {
            let container = DxbcContainer::parse(bytes)?;
            container
                .get(FourCC::SHDR)
                .or_else(|| container.get(FourCC::SHEX))
                .map(|chunk| chunk.data)
                .ok_or(D3d9ShaderCacheError::DxbcMissingShaderChunk)
        }
    }
}

