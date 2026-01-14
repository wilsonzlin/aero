use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use aero_gpu::bindings::bind_group_cache::{BindGroupCache, BufferId, TextureViewId};
use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
use aero_gpu::bindings::samplers::SamplerCache;
use aero_gpu::indirect::{DrawIndexedIndirectArgs, DrawIndirectArgs};
use aero_gpu::passthrough_vs::PassthroughVertexShaderKey;
use aero_gpu::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use aero_gpu::pipeline_key::{
    ColorTargetKey, RenderPipelineKey, ShaderHash, ShaderStage, VertexAttributeKey,
    VertexBufferLayoutKey,
};
use aero_gpu::stats::PipelineCacheStats;
use aero_gpu::GpuCapabilities;
use anyhow::{anyhow, bail, Context, Result};

use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    VertexBufferLayoutOwned, VsInputSignatureElement, MAX_INPUT_SLOTS,
};
use crate::sm4::opcode as sm4_opcode;
use crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap;
use crate::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, ShaderReflection, Sm4Program,
};

use super::aerogpu_state::{
    AerogpuHandle, BlendState, D3D11ShadowState, DepthStencilState, IndexBufferBinding,
    PrimitiveTopology, RasterizerState, ScissorRect, VertexBufferBinding, Viewport,
};
use super::pipeline_layout_cache::PipelineLayoutCache;
use super::reflection_bindings;

#[derive(Debug)]
pub struct BufferResource {
    pub buffer: wgpu::Buffer,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

#[derive(Debug)]
pub struct TextureResource {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub desc: Texture2dDesc,
}

#[derive(Debug, Clone)]
pub struct ShaderResource {
    pub stage: ShaderStage,
    pub wgsl: String,
    pub hash: ShaderHash,
    pub vs_input_signature: Vec<VsInputSignatureElement>,
    pub reflection: ShaderReflection,
}

#[derive(Debug, Clone)]
pub struct InputLayoutResource {
    layout: InputLayoutDesc,
}

#[derive(Debug, Default)]
pub struct AerogpuResources {
    buffers: HashMap<AerogpuHandle, BufferResource>,
    textures: HashMap<AerogpuHandle, TextureResource>,
    samplers: HashMap<AerogpuHandle, aero_gpu::bindings::samplers::CachedSampler>,
    shaders: HashMap<AerogpuHandle, ShaderResource>,
    input_layouts: HashMap<AerogpuHandle, InputLayoutResource>,
}

const DEFAULT_BIND_GROUP_CACHE_CAPACITY: usize = 4096;
const DUMMY_UNIFORM_SIZE_BYTES: u64 = 4096 * 16;

/// A minimal `aerogpu_cmd`-style executor focused on D3D10/11 rendering.
///
/// The guest streams D3D11-style incremental state updates; when a draw is
/// issued we derive a [`RenderPipelineKey`] from the current [`D3D11ShadowState`]
/// and use [`PipelineCache`] to materialize `wgpu` pipelines on demand.
pub struct AerogpuCmdRuntime {
    device: wgpu::Device,
    queue: wgpu::Queue,
    supports_indirect_execution: bool,

    pub state: D3D11ShadowState,
    pub resources: AerogpuResources,
    pipelines: PipelineCache,
    pipeline_layout_cache: PipelineLayoutCache<Arc<wgpu::PipelineLayout>>,

    dummy_uniform: wgpu::Buffer,
    dummy_storage: wgpu::Buffer,
    dummy_texture_view: wgpu::TextureView,
    sampler_cache: SamplerCache,
    default_sampler: aero_gpu::bindings::samplers::CachedSampler,
    bind_group_layout_cache: BindGroupLayoutCache,
    bind_group_cache: BindGroupCache<Arc<wgpu::BindGroup>>,
}

impl AerogpuCmdRuntime {
    pub async fn new_for_tests() -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);
            if needs_runtime_dir {
                let dir = std::env::temp_dir().join(format!(
                    "aero-d3d11-aerogpu-xdg-runtime-{}",
                    std::process::id()
                ));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
                // Prefer "native" backends; this avoids noisy platform warnings from
                // initializing GL/WAYLAND stacks in headless CI environments.
                wgpu::Backends::PRIMARY
            },
            ..Default::default()
        });
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
        {
            Some(adapter) => Some(adapter),
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        }
        .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

        let downlevel = adapter.get_downlevel_capabilities();
        let supports_indirect_execution =
            super::supports_indirect_execution_from_downlevel_flags(downlevel.flags);
        let requested_features = super::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 aerogpu test device"),
                    required_features: requested_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        let caps = GpuCapabilities::from_device(&device).with_downlevel_flags(downlevel.flags);
        let pipelines = PipelineCache::new(PipelineCacheConfig::default(), caps);

        let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu dummy uniform"),
            size: DUMMY_UNIFORM_SIZE_BYTES,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &dummy_uniform,
            0,
            &vec![0u8; DUMMY_UNIFORM_SIZE_BYTES as usize],
        );

        let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu dummy storage"),
            size: 4096,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&dummy_storage, 0, &vec![0u8; 4096]);

        let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d11 aerogpu dummy texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let dummy_texture_view = dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &dummy_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0, 0, 0, 255],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        let mut sampler_cache = SamplerCache::new();
        let default_sampler = sampler_cache.get_or_create(
            &device,
            &wgpu::SamplerDescriptor {
                label: Some("aero-d3d11 aerogpu default sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                lod_min_clamp: 0.0,
                lod_max_clamp: 32.0,
                compare: None,
                anisotropy_clamp: 1,
                border_color: None,
            },
        );

        Ok(Self {
            device,
            queue,
            supports_indirect_execution,
            state: D3D11ShadowState::default(),
            resources: AerogpuResources::default(),
            pipelines,
            pipeline_layout_cache: PipelineLayoutCache::new(),
            dummy_uniform,
            dummy_storage,
            dummy_texture_view,
            sampler_cache,
            default_sampler,
            bind_group_layout_cache: BindGroupLayoutCache::new(),
            bind_group_cache: BindGroupCache::new(DEFAULT_BIND_GROUP_CACHE_CAPACITY),
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn supports_indirect_execution(&self) -> bool {
        self.supports_indirect_execution
    }

    pub fn poll_wait(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);
    }

    pub fn pipeline_cache_stats(&self) -> PipelineCacheStats {
        self.pipelines.stats()
    }

    pub fn pipeline_layout_cache_stats(&self) -> aero_gpu::bindings::CacheStats {
        self.pipeline_layout_cache.stats()
    }

    pub fn create_buffer(&mut self, handle: AerogpuHandle, size: u64, usage: wgpu::BufferUsages) {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu buffer"),
            size,
            usage,
            mapped_at_creation: false,
        });
        self.resources
            .buffers
            .insert(handle, BufferResource { buffer, size });
    }

    pub fn write_buffer(&self, handle: AerogpuHandle, offset: u64, data: &[u8]) -> Result<()> {
        let buf = self
            .resources
            .buffers
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown buffer handle {handle}"))?;
        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        let size_bytes = data.len() as u64;
        if !offset.is_multiple_of(alignment) || !size_bytes.is_multiple_of(alignment) {
            bail!(
                "write_buffer: offset and size must be {alignment}-byte aligned (offset={offset} size_bytes={size_bytes})"
            );
        }
        if offset.saturating_add(data.len() as u64) > buf.size {
            bail!(
                "write_buffer out of bounds: offset={} len={} size={}",
                offset,
                data.len(),
                buf.size
            );
        }
        self.queue.write_buffer(&buf.buffer, offset, data);
        Ok(())
    }

    pub fn create_texture2d(
        &mut self,
        handle: AerogpuHandle,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d11 aerogpu texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.resources.textures.insert(
            handle,
            TextureResource {
                texture,
                view,
                desc: Texture2dDesc {
                    width,
                    height,
                    format,
                },
            },
        );
    }

    pub fn create_sampler(
        &mut self,
        handle: AerogpuHandle,
        desc: &wgpu::SamplerDescriptor<'_>,
    ) -> Result<()> {
        let sampler = self.sampler_cache.get_or_create(&self.device, desc);
        self.resources.samplers.insert(handle, sampler);
        Ok(())
    }

    pub fn write_texture_rgba8(
        &self,
        handle: AerogpuHandle,
        width: u32,
        height: u32,
        bytes_per_row: u32,
        data: &[u8],
    ) -> Result<()> {
        let tex = self
            .resources
            .textures
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown texture handle {handle}"))?;

        if tex.desc.format != wgpu::TextureFormat::Rgba8Unorm {
            bail!(
                "write_texture_rgba8: only supports Rgba8Unorm (got {:?})",
                tex.desc.format
            );
        }
        if tex.desc.width != width || tex.desc.height != height {
            bail!(
                "write_texture_rgba8: size mismatch (expected {}x{}, got {}x{})",
                tex.desc.width,
                tex.desc.height,
                width,
                height
            );
        }

        let unpadded_bpr = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("write_texture_rgba8: bytes_per_row overflow"))?;
        if bytes_per_row < unpadded_bpr {
            bail!(
                "write_texture_rgba8: bytes_per_row too small (bytes_per_row={bytes_per_row} required={unpadded_bpr})"
            );
        }

        let required_len = bytes_per_row
            .checked_mul(height)
            .ok_or_else(|| anyhow!("write_texture_rgba8: data len overflow"))?
            as usize;
        if data.len() < required_len {
            bail!(
                "write_texture_rgba8: data too small (len={} required={})",
                data.len(),
                required_len
            );
        }

        let aligned = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let repacked = if height > 1 && !bytes_per_row.is_multiple_of(aligned) {
            let padded_bpr = unpadded_bpr.div_ceil(aligned) * aligned;
            let mut repacked = vec![0u8; (padded_bpr * height) as usize];
            for row in 0..height as usize {
                let src = row * bytes_per_row as usize;
                let dst = row * padded_bpr as usize;
                repacked[dst..dst + unpadded_bpr as usize]
                    .copy_from_slice(&data[src..src + unpadded_bpr as usize]);
            }
            Some((padded_bpr, repacked))
        } else {
            None
        };

        let (bpr, bytes) = match repacked.as_ref() {
            Some((padded_bpr, repacked)) => (*padded_bpr, repacked.as_slice()),
            None => (bytes_per_row, &data[..required_len]),
        };

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        Ok(())
    }

    pub fn create_shader_dxbc(&mut self, handle: AerogpuHandle, dxbc_bytes: &[u8]) -> Result<()> {
        let dxbc = DxbcFile::parse(dxbc_bytes).context("parse DXBC")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("parse DXBC shader chunk")?;

        // SM5 geometry shaders can emit to multiple output streams via `emit_stream` / `cut_stream`.
        // Aero's initial GS bring-up only targets stream 0, so reject any shaders that use a
        // non-zero stream index with a clear diagnostic.
        //
        // Validate before stage dispatch so the policy is enforced even for GS/HS/DS shaders that
        // are currently accepted-but-ignored by the WebGPU backend.
        validate_sm5_gs_streams(&program)?;

        let stage = match program.stage {
            crate::ShaderStage::Vertex => ShaderStage::Vertex,
            crate::ShaderStage::Pixel => ShaderStage::Fragment,
            // Geometry/hull/domain stages are not supported by the AeroGPU/WebGPU pipeline.
            //
            // The command stream includes a geometry stage slot for ABI compatibility, but the
            // stage is ignored by the host today. Accept the create call (so guests can
            // compile/pass these shaders around) but ignore the resulting shader.
            crate::ShaderStage::Geometry
            | crate::ShaderStage::Hull
            | crate::ShaderStage::Domain => {
                eprintln!(
                    "aero-d3d11 aerogpu_cmd_runtime: ignoring unsupported shader stage {:?} for handle {}",
                    program.stage, handle
                );
                return Ok(());
            }
            // This `AerogpuCmdRuntime` is currently render-only and does not expose a compute
            // dispatch path. Real DXBC blobs may still include compute shaders, so accept the
            // create call for robustness but ignore the shader for now.
            crate::ShaderStage::Compute => {
                return Ok(());
            }
            other => bail!("unsupported shader stage for aerogpu_cmd executor: {other:?}"),
        };

        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;

        // Future-proofing for SM5 geometry shader output streams:
        //
        // DXBC signatures include a `stream` field which is used by geometry shader multi-stream
        // output (and stream-out). Our rasterization pipeline currently only supports stream 0, so
        // reject shaders that declare non-zero streams to avoid silently rasterizing the wrong
        // stream.
        if let Some(osgn) = signatures.osgn.as_ref() {
            for p in &osgn.parameters {
                if p.stream != 0 {
                    bail!(
                        "create_shader_dxbc: output signature parameter {}{} (r{}) is declared on stream {} (only stream 0 is supported)",
                        p.semantic_name,
                        p.semantic_index,
                        p.register,
                        p.stream
                    );
                }
            }
        }

        let signature_driven = signatures.isgn.is_some() && signatures.osgn.is_some();
        let (wgsl, reflection, vs_input_signature) = if signature_driven {
            let module =
                crate::sm4::decode_program(&program).context("decode SM4/5 token stream")?;
            let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
                .context("translate WGSL")?;

            let vs_input_signature = if stage == ShaderStage::Vertex {
                extract_vs_input_signature_unique_locations(&signatures, &module)
                    .context("extract VS input signature")?
            } else {
                Vec::new()
            };

            (translated.wgsl, translated.reflection, vs_input_signature)
        } else {
            let wgsl = translate_sm4_to_wgsl_bootstrap(&program)
                .context("translate SM4/5 to WGSL")?
                .wgsl;
            let vs_input_signature = if stage == ShaderStage::Vertex {
                extract_vs_input_signature(&signatures).context("extract VS input signature")?
            } else {
                Vec::new()
            };
            (wgsl, ShaderReflection::default(), vs_input_signature)
        };

        // Register into the shared PipelineCache shader-module cache.
        let (hash, _module) = self.pipelines.get_or_create_shader_module(
            &self.device,
            stage,
            &wgsl,
            Some("aerogpu shader"),
        );

        self.resources.shaders.insert(
            handle,
            ShaderResource {
                stage,
                wgsl,
                hash,
                vs_input_signature,
                reflection,
            },
        );
        Ok(())
    }

    pub fn create_input_layout(&mut self, handle: AerogpuHandle, blob: &[u8]) -> Result<()> {
        let layout = InputLayoutDesc::parse(blob)
            .map_err(|e| anyhow!("failed to parse ILAY input layout blob: {e}"))?;
        self.resources
            .input_layouts
            .insert(handle, InputLayoutResource { layout });
        Ok(())
    }

    pub fn bind_shaders(
        &mut self,
        vs: Option<AerogpuHandle>,
        gs: Option<AerogpuHandle>,
        ps: Option<AerogpuHandle>,
    ) {
        self.state.vs = vs;
        self.state.gs = gs;
        self.state.ps = ps;

        if let Some(handle) = gs {
            eprintln!(
                "aero-d3d11 aerogpu_cmd_runtime: geometry shader {handle} bound but geometry shaders are not supported; ignoring"
            );
        }
    }

    pub fn set_input_layout(&mut self, layout: Option<AerogpuHandle>) {
        self.state.input_layout = layout;
    }

    pub fn set_vertex_buffers(&mut self, start_slot: usize, bindings: &[VertexBufferBinding]) {
        let end = start_slot.saturating_add(bindings.len());
        if end > self.state.vertex_buffers.len() {
            self.state.vertex_buffers.resize(end, None);
        }
        for (i, binding) in bindings.iter().enumerate() {
            self.state.vertex_buffers[start_slot + i] = Some(*binding);
        }
    }

    pub fn set_index_buffer(&mut self, binding: Option<IndexBufferBinding>) {
        self.state.index_buffer = binding;
    }

    pub fn set_primitive_topology(&mut self, topology: PrimitiveTopology) {
        self.state.primitive_topology = topology;
    }

    pub fn set_render_targets(
        &mut self,
        colors: &[Option<AerogpuHandle>; 8],
        depth_stencil: Option<AerogpuHandle>,
    ) {
        self.state.render_targets.colors = *colors;
        self.state.render_targets.depth_stencil = depth_stencil;
    }

    pub fn set_viewport(&mut self, viewport: Option<Viewport>) {
        self.state.viewport = viewport;
    }

    pub fn set_scissor(&mut self, scissor: Option<ScissorRect>) {
        self.state.scissor = scissor;
    }

    pub fn set_blend_state(&mut self, state: BlendState) {
        self.state.blend_state = state;
    }

    pub fn set_depth_stencil_state(&mut self, state: DepthStencilState) {
        self.state.depth_stencil_state = state;
    }

    pub fn set_rasterizer_state(&mut self, state: RasterizerState) {
        self.state.rasterizer_state = state;
    }

    pub fn set_vs_constant_buffer(&mut self, slot: u32, buffer: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.vs.constant_buffers.len() <= slot {
            self.state
                .bindings
                .vs
                .constant_buffers
                .resize(slot + 1, None);
        }
        self.state.bindings.vs.constant_buffers[slot] = buffer;
    }

    pub fn set_ps_constant_buffer(&mut self, slot: u32, buffer: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.ps.constant_buffers.len() <= slot {
            self.state
                .bindings
                .ps
                .constant_buffers
                .resize(slot + 1, None);
        }
        self.state.bindings.ps.constant_buffers[slot] = buffer;
    }

    pub fn set_gs_constant_buffer(&mut self, slot: u32, buffer: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.gs.constant_buffers.len() <= slot {
            self.state
                .bindings
                .gs
                .constant_buffers
                .resize(slot + 1, None);
        }
        self.state.bindings.gs.constant_buffers[slot] = buffer;
    }

    pub fn set_vs_texture(&mut self, slot: u32, texture: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.vs.textures.len() <= slot {
            self.state.bindings.vs.textures.resize(slot + 1, None);
        }
        self.state.bindings.vs.textures[slot] = texture;
    }

    pub fn set_ps_texture(&mut self, slot: u32, texture: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.ps.textures.len() <= slot {
            self.state.bindings.ps.textures.resize(slot + 1, None);
        }
        self.state.bindings.ps.textures[slot] = texture;
    }

    pub fn set_gs_texture(&mut self, slot: u32, texture: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.gs.textures.len() <= slot {
            self.state.bindings.gs.textures.resize(slot + 1, None);
        }
        self.state.bindings.gs.textures[slot] = texture;
    }

    pub fn set_vs_sampler(&mut self, slot: u32, sampler: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.vs.samplers.len() <= slot {
            self.state.bindings.vs.samplers.resize(slot + 1, None);
        }
        self.state.bindings.vs.samplers[slot] = sampler;
    }

    pub fn set_ps_sampler(&mut self, slot: u32, sampler: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.ps.samplers.len() <= slot {
            self.state.bindings.ps.samplers.resize(slot + 1, None);
        }
        self.state.bindings.ps.samplers[slot] = sampler;
    }

    pub fn set_gs_sampler(&mut self, slot: u32, sampler: Option<AerogpuHandle>) {
        let slot = slot as usize;
        if self.state.bindings.gs.samplers.len() <= slot {
            self.state.bindings.gs.samplers.resize(slot + 1, None);
        }
        self.state.bindings.gs.samplers[slot] = sampler;
    }

    pub fn draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) -> Result<()> {
        self.draw_internal(DrawKind::NonIndexed(DrawIndirectArgs {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        }))
    }

    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    ) -> Result<()> {
        self.draw_internal(DrawKind::Indexed(DrawIndexedIndirectArgs {
            index_count,
            instance_count,
            first_index,
            base_vertex,
            first_instance,
        }))
    }

    /// Draw using an expanded-geometry vertex buffer and a generated passthrough vertex shader.
    ///
    /// This bypasses the currently bound vertex shader + input layout and instead treats
    /// `expanded_vertex_buffer` as a tightly-packed vertex buffer containing:
    ///
    /// - `vec4<f32>` clip-space position
    /// - one `vec4<f32>` for each `@location(N)` varying declared by the currently-bound pixel
    ///   shader (in ascending location order)
    ///
    /// The pixel shader is taken from `self.state.ps`.
    pub fn draw_expanded_passthrough(
        &mut self,
        expanded_vertex_buffer: AerogpuHandle,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) -> Result<()> {
        self.draw_expanded_passthrough_internal(
            expanded_vertex_buffer,
            DrawKind::NonIndexed(DrawIndirectArgs {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            }),
        )
    }

    fn draw_internal(&mut self, kind: DrawKind) -> Result<()> {
        let vs_handle = self
            .state
            .vs
            .ok_or_else(|| anyhow!("draw without a bound VS"))?;
        let ps_handle = self
            .state
            .ps
            .ok_or_else(|| anyhow!("draw without a bound PS"))?;

        if let Some(handle) = self.state.gs {
            eprintln!(
                "aero-d3d11 aerogpu_cmd_runtime: geometry shader {handle} is currently ignored"
            );
        }

        let vs = self
            .resources
            .shaders
            .get(&vs_handle)
            .ok_or_else(|| anyhow!("unknown VS handle {vs_handle}"))?;
        let ps = self
            .resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS handle {ps_handle}"))?;
        if vs.stage != ShaderStage::Vertex {
            bail!("shader {vs_handle} is not a vertex shader");
        }
        if ps.stage != ShaderStage::Fragment {
            bail!("shader {ps_handle} is not a pixel/fragment shader");
        }

        // WebGPU requires that the vertex output interface exactly matches the fragment input
        // interface. D3D shaders, however, frequently export varyings that a given pixel shader does
        // not consume (because the same VS can be reused with multiple PS variants), and pixel
        // shaders may declare inputs they never read.
        //
        // To preserve D3D behavior, we trim the stage interface at pipeline-creation time:
        // - Drop unused pixel shader inputs when the bound VS does not output them.
        // - Drop unused vertex shader outputs that the pixel shader does not declare.
        let ps_declared_inputs = super::wgsl_link::locations_in_struct(&ps.wgsl, "PsIn")?;
        let vs_output_locations = super::wgsl_link::locations_in_struct(&vs.wgsl, "VsOut")?;

        let mut ps_link_locations = ps_declared_inputs.clone();
        let ps_missing_locations: BTreeSet<u32> = ps_declared_inputs
            .difference(&vs_output_locations)
            .copied()
            .collect();
        if !ps_missing_locations.is_empty() {
            let ps_used_locations = super::wgsl_link::referenced_ps_input_locations(&ps.wgsl);
            let used_missing: Vec<u32> = ps_missing_locations
                .intersection(&ps_used_locations)
                .copied()
                .collect();
            if let Some(&loc) = used_missing.first() {
                bail!("pixel shader reads @location({loc}), but VS does not output it");
            }

            ps_link_locations = ps_declared_inputs
                .intersection(&vs_output_locations)
                .copied()
                .collect();
        }

        let mut linked_ps_wgsl = std::borrow::Cow::Borrowed(ps.wgsl.as_str());
        if ps_link_locations != ps_declared_inputs {
            linked_ps_wgsl =
                std::borrow::Cow::Owned(super::wgsl_link::trim_ps_inputs_to_locations(
                    linked_ps_wgsl.as_ref(),
                    &ps_link_locations,
                ));
        }

        // WebGPU requires that the fragment shader's `@location(N)` outputs line up with the render
        // pipeline's `ColorTargetState` array. D3D discards writes to unbound RTVs instead.
        //
        // To emulate D3D, trim fragment outputs to the set of currently bound render target slots.
        let mut keep_output_locations = BTreeSet::new();
        for (slot, handle) in self.state.render_targets.colors.iter().enumerate() {
            if handle.is_some() {
                keep_output_locations.insert(slot as u32);
            }
        }

        let declared_outputs =
            super::wgsl_link::declared_ps_output_locations(linked_ps_wgsl.as_ref())?;
        let missing_outputs: BTreeSet<u32> = declared_outputs
            .difference(&keep_output_locations)
            .copied()
            .collect();
        if !missing_outputs.is_empty() {
            linked_ps_wgsl =
                std::borrow::Cow::Owned(super::wgsl_link::trim_ps_outputs_to_locations(
                    linked_ps_wgsl.as_ref(),
                    &keep_output_locations,
                ));
        }

        let linked_ps_hash = if linked_ps_wgsl.as_ref() == ps.wgsl.as_str() {
            ps.hash
        } else {
            let (hash, _module) = self.pipelines.get_or_create_shader_module(
                &self.device,
                ShaderStage::Fragment,
                linked_ps_wgsl.as_ref(),
                Some("aero-d3d11 aerogpu linked fragment shader"),
            );
            hash
        };

        let linked_vs_hash = if vs_output_locations == ps_link_locations {
            vs.hash
        } else {
            let linked_vs_wgsl =
                super::wgsl_link::trim_vs_outputs_to_locations(&vs.wgsl, &ps_link_locations);
            let (hash, _module) = self.pipelines.get_or_create_shader_module(
                &self.device,
                ShaderStage::Vertex,
                &linked_vs_wgsl,
                Some("aero-d3d11 aerogpu linked vertex shader"),
            );
            hash
        };

        let (color_attachments, color_target_keys, color_size) =
            build_color_attachments(&self.resources, &self.state)?;

        let (depth_attachment, depth_target_key, depth_state, depth_size) =
            build_depth_attachment(&self.resources, &self.state)?;

        let target_size = color_size
            .or(depth_size)
            .ok_or_else(|| anyhow!("draw without bound render targets"))?;

        let primitive_topology = map_topology(self.state.primitive_topology)?;
        let strip_index_format = match primitive_topology {
            wgpu::PrimitiveTopology::LineStrip | wgpu::PrimitiveTopology::TriangleStrip => {
                self.state.index_buffer.map(|ib| ib.format)
            }
            _ => None,
        };
        let cull_mode = self.state.rasterizer_state.cull_mode;
        let front_face = self.state.rasterizer_state.front_face;
        let scissor_enabled = self.state.rasterizer_state.scissor_enable;

        let BuiltVertexState {
            layouts: owned_vertex_layouts,
            keys: vertex_buffer_keys,
            wgpu_slot_to_d3d_slot,
        } = build_vertex_state(&self.resources, &self.state, &vs.vs_input_signature)?;

        // `owned_vertex_layouts` is moved into the pipeline-creation closure below, but we still
        // need per-slot stride/step-mode information later to clamp indirect draw args against the
        // currently bound vertex buffers. Extract a lightweight copy here.
        let vertex_slot_info: Vec<(u64, wgpu::VertexStepMode)> = owned_vertex_layouts
            .iter()
            .map(|l| (l.array_stride, l.step_mode))
            .collect();

        let pipeline_bindings = reflection_bindings::build_pipeline_bindings_info(
            &self.device,
            &mut self.bind_group_layout_cache,
            [
                reflection_bindings::ShaderBindingSet::Guest(vs.reflection.bindings.as_slice()),
                reflection_bindings::ShaderBindingSet::Guest(ps.reflection.bindings.as_slice()),
            ],
            reflection_bindings::BindGroupIndexValidation::GuestShaders,
        )?;
        let reflection_bindings::PipelineBindingsInfo {
            layout_key,
            group_layouts,
            group_bindings,
        } = pipeline_bindings;

        let pipeline_layout = {
            let device = &self.device;
            let cache = &mut self.pipeline_layout_cache;
            cache.get_or_create_with(&layout_key, || {
                let layout_refs: Vec<&wgpu::BindGroupLayout> =
                    group_layouts.iter().map(|l| l.layout.as_ref()).collect();
                Arc::new(
                    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aero-d3d11 aerogpu pipeline layout"),
                        bind_group_layouts: &layout_refs,
                        push_constant_ranges: &[],
                    }),
                )
            })
        };

        let key = RenderPipelineKey {
            vertex_shader: linked_vs_hash,
            fragment_shader: linked_ps_hash,
            color_targets: color_target_keys,
            depth_stencil: depth_target_key,
            primitive_topology,
            strip_index_format,
            cull_mode,
            front_face,
            vertex_buffers: vertex_buffer_keys,
            sample_count: 1,
            layout: layout_key,
        };

        let blend = self.state.blend_state;
        let mut color_target_states: Vec<Option<wgpu::ColorTargetState>> = Vec::new();
        for ct in &key.color_targets {
            let Some(ct) = ct else {
                color_target_states.push(None);
                continue;
            };
            color_target_states.push(Some(wgpu::ColorTargetState {
                format: ct.format,
                blend: blend.blend,
                write_mask: ct.write_mask,
            }));
        }

        let depth_stencil_state = depth_state.clone();

        // Fetch or create pipeline.
        let pipeline = self.pipelines.get_or_create_render_pipeline(
            &self.device,
            key,
            move |device, vs_module, fs_module| {
                let pipeline_layout = pipeline_layout.as_ref();

                let vertex_buffers: Vec<wgpu::VertexBufferLayout<'_>> = owned_vertex_layouts
                    .iter()
                    .map(|l| wgpu::VertexBufferLayout {
                        array_stride: l.array_stride,
                        step_mode: l.step_mode,
                        attributes: &l.attributes,
                    })
                    .collect();

                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("aero-d3d11 aerogpu render pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: vs_module,
                        entry_point: "vs_main",
                        buffers: &vertex_buffers,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: fs_module,
                        entry_point: "fs_main",
                        targets: &color_target_states,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: primitive_topology,
                        strip_index_format,
                        front_face,
                        cull_mode,
                        ..Default::default()
                    },
                    depth_stencil: depth_stencil_state,
                    multisample: wgpu::MultisampleState {
                        count: 1,
                        ..Default::default()
                    },
                    multiview: None,
                })
            },
        )?;

        let mut bind_groups: Vec<Arc<wgpu::BindGroup>> = Vec::with_capacity(group_layouts.len());
        let has_geometry_group = group_layouts.len() > 2;
        for (group_index, (layout, bindings)) in
            group_layouts.iter().zip(group_bindings.iter()).enumerate()
        {
            let stage_state = match group_index as u32 {
                0 => Some(&self.state.bindings.vs),
                1 => Some(if has_geometry_group {
                    &self.state.bindings.gs
                } else {
                    &self.state.bindings.ps
                }),
                2 => has_geometry_group.then_some(&self.state.bindings.ps),
                _ => None,
            };
            let provider = RuntimeBindGroupProvider {
                resources: &self.resources,
                stage_state,
                dummy_uniform: &self.dummy_uniform,
                dummy_storage: &self.dummy_storage,
                dummy_texture_view: &self.dummy_texture_view,
                default_sampler: &self.default_sampler,
            };
            bind_groups.push(reflection_bindings::build_bind_group(
                &self.device,
                &mut self.bind_group_cache,
                layout,
                bindings,
                &provider,
            )?);
        }

        // Encode the draw.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 aerogpu draw encoder"),
            });

        // If we end up using indirect draws, we allocate a temporary args buffer and keep it alive
        // until the render pass is dropped.
        //
        // NOTE: This must be declared *before* `pass` so drop order ensures the buffer outlives the
        // render pass.
        let mut indirect_args_buffer: Option<wgpu::Buffer> = None;

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aero-d3d11 aerogpu draw pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        pass.set_pipeline(pipeline);

        for (group_index, bind_group) in bind_groups.iter().enumerate() {
            pass.set_bind_group(group_index as u32, bind_group.as_ref(), &[]);
        }

        // Viewport/scissor are dynamic state; apply on every draw.
        let mut skip_draw = false;
        let default_viewport = Viewport {
            x: 0.0,
            y: 0.0,
            width: target_size.0 as f32,
            height: target_size.1 as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let viewport_opt = self.state.viewport;
        let mut viewport = viewport_opt.unwrap_or(default_viewport);
        if !viewport.x.is_finite()
            || !viewport.y.is_finite()
            || !viewport.width.is_finite()
            || !viewport.height.is_finite()
            || !viewport.min_depth.is_finite()
            || !viewport.max_depth.is_finite()
        {
            viewport = default_viewport;
        }

        let max_w = target_size.0 as f32;
        let max_h = target_size.1 as f32;
        let left = viewport.x.max(0.0);
        let top = viewport.y.max(0.0);
        let right = (viewport.x + viewport.width).max(0.0).min(max_w);
        let bottom = (viewport.y + viewport.height).max(0.0).min(max_h);
        let width = (right - left).max(0.0);
        let height = (bottom - top).max(0.0);

        let mut viewport_empty = false;
        if viewport_opt.is_some() && (viewport.width <= 0.0 || viewport.height <= 0.0) {
            viewport_empty = true;
        }
        if !viewport_empty && width > 0.0 && height > 0.0 {
            let mut min_depth = viewport.min_depth.clamp(0.0, 1.0);
            let mut max_depth = viewport.max_depth.clamp(0.0, 1.0);
            if min_depth > max_depth {
                std::mem::swap(&mut min_depth, &mut max_depth);
            }
            pass.set_viewport(left, top, width, height, min_depth, max_depth);
        } else if viewport_opt.is_some() {
            viewport_empty = true;
        }

        let mut scissor_empty = false;
        if scissor_enabled {
            if let Some(scissor) = self.state.scissor {
                let x = scissor.x.min(target_size.0);
                let y = scissor.y.min(target_size.1);
                let width = scissor.width.min(target_size.0.saturating_sub(x));
                let height = scissor.height.min(target_size.1.saturating_sub(y));
                if width > 0 && height > 0 {
                    pass.set_scissor_rect(x, y, width, height);
                } else {
                    scissor_empty = true;
                    pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
                }
            } else {
                // Scissor test enabled but no scissor set -> treat as full target.
                pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
            }
        } else {
            pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
        }

        if viewport_empty || scissor_empty {
            skip_draw = true;
        }

        for (wgpu_slot, d3d_slot) in wgpu_slot_to_d3d_slot.iter().copied().enumerate() {
            let slot = d3d_slot as usize;
            let Some(binding) = self.state.vertex_buffers.get(slot).and_then(|b| *b) else {
                bail!("vertex buffer slot {d3d_slot} is required by input layout but not bound");
            };
            let buf = self
                .resources
                .buffers
                .get(&binding.buffer)
                .ok_or_else(|| anyhow!("unknown vertex buffer {}", binding.buffer))?;
            pass.set_vertex_buffer(wgpu_slot as u32, buf.buffer.slice(binding.offset..));
        }

        let indirect_first_instance_supported = self
            .device
            .features()
            .contains(wgpu::Features::INDIRECT_FIRST_INSTANCE);

        if !skip_draw {
            match kind {
                DrawKind::NonIndexed(mut args) => {
                    // Clamp against vertex buffers referenced by the current pipeline to avoid
                    // out-of-bounds vertex fetches.
                    let mut max_vertices: Option<u32> = None;
                    let mut max_instances: Option<u32> = None;
                    for (wgpu_slot, d3d_slot) in wgpu_slot_to_d3d_slot.iter().copied().enumerate() {
                        let slot = d3d_slot as usize;
                        let Some(binding) = self.state.vertex_buffers.get(slot).and_then(|b| *b)
                        else {
                            continue;
                        };
                        let buf = match self.resources.buffers.get(&binding.buffer) {
                            Some(buf) => buf,
                            None => continue,
                        };
                        let (stride, step_mode) = vertex_slot_info
                            .get(wgpu_slot)
                            .copied()
                            .expect("wgpu_slot_to_d3d_slot and vertex_slot_info must match");
                        let available = buf.size.saturating_sub(binding.offset);
                        let max = aero_gpu::indirect::max_elements_in_buffer(available, stride);
                        match step_mode {
                            wgpu::VertexStepMode::Vertex => {
                                max_vertices = Some(max_vertices.map_or(max, |cur| cur.min(max)));
                            }
                            wgpu::VertexStepMode::Instance => {
                                max_instances = Some(max_instances.map_or(max, |cur| cur.min(max)));
                            }
                        }
                    }
                    if let Some(max) = max_vertices {
                        args.clamp_vertices(max);
                    }
                    if let Some(max) = max_instances {
                        args.clamp_instances(max);
                    }

                    if args.first_instance != 0 && !indirect_first_instance_supported {
                        // Downlevel backends (notably wgpu GL) may not support indirect
                        // first-instance. Fall back to direct draws to preserve semantics.
                        let end_vertex = args.first_vertex.saturating_add(args.vertex_count);
                        let end_instance = args.first_instance.saturating_add(args.instance_count);
                        pass.draw(
                            args.first_vertex..end_vertex,
                            args.first_instance..end_instance,
                        );
                        indirect_args_buffer = None;
                    } else {
                        indirect_args_buffer =
                            Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("aero-d3d11 aerogpu draw_indirect args"),
                                size: DrawIndirectArgs::SIZE_BYTES,
                                usage: wgpu::BufferUsages::INDIRECT
                                    | wgpu::BufferUsages::COPY_DST
                                    | wgpu::BufferUsages::STORAGE,
                                mapped_at_creation: false,
                            }));
                        let args_buffer = indirect_args_buffer
                            .as_ref()
                            .expect("indirect_args_buffer must be set");
                        self.queue.write_buffer(args_buffer, 0, args.as_bytes());
                        pass.draw_indirect(args_buffer, 0);
                    }
                }
                DrawKind::Indexed(mut args) => {
                    let index = self
                        .state
                        .index_buffer
                        .ok_or_else(|| anyhow!("DrawIndexed without a bound index buffer"))?;
                    let buf = self
                        .resources
                        .buffers
                        .get(&index.buffer)
                        .ok_or_else(|| anyhow!("unknown index buffer {}", index.buffer))?;
                    pass.set_index_buffer(buf.buffer.slice(index.offset..), index.format);

                    // Clamp against index buffer size (avoids OOB index fetch).
                    let index_stride = match index.format {
                        wgpu::IndexFormat::Uint16 => 2u64,
                        wgpu::IndexFormat::Uint32 => 4u64,
                    };
                    let available = buf.size.saturating_sub(index.offset);
                    let max_indices =
                        aero_gpu::indirect::max_elements_in_buffer(available, index_stride);
                    args.clamp_indices(max_indices);

                    // Clamp instance count for any instance-rate vertex buffers (best-effort).
                    let mut max_instances: Option<u32> = None;
                    for (wgpu_slot, d3d_slot) in wgpu_slot_to_d3d_slot.iter().copied().enumerate() {
                        let (stride, step_mode) = vertex_slot_info
                            .get(wgpu_slot)
                            .copied()
                            .expect("wgpu_slot_to_d3d_slot and vertex_slot_info must match");
                        if step_mode != wgpu::VertexStepMode::Instance {
                            continue;
                        }
                        let slot = d3d_slot as usize;
                        let Some(binding) = self.state.vertex_buffers.get(slot).and_then(|b| *b)
                        else {
                            continue;
                        };
                        let buf = match self.resources.buffers.get(&binding.buffer) {
                            Some(buf) => buf,
                            None => continue,
                        };
                        let available = buf.size.saturating_sub(binding.offset);
                        let max = aero_gpu::indirect::max_elements_in_buffer(available, stride);
                        max_instances = Some(max_instances.map_or(max, |cur| cur.min(max)));
                    }
                    if let Some(max) = max_instances {
                        args.clamp_instances(max);
                    }

                    if args.first_instance != 0 && !indirect_first_instance_supported {
                        let end_index = args.first_index.saturating_add(args.index_count);
                        let end_instance = args.first_instance.saturating_add(args.instance_count);
                        pass.draw_indexed(
                            args.first_index..end_index,
                            args.base_vertex,
                            args.first_instance..end_instance,
                        );
                        indirect_args_buffer = None;
                    } else {
                        indirect_args_buffer =
                            Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("aero-d3d11 aerogpu draw_indexed_indirect args"),
                                size: DrawIndexedIndirectArgs::SIZE_BYTES,
                                usage: wgpu::BufferUsages::INDIRECT
                                    | wgpu::BufferUsages::COPY_DST
                                    | wgpu::BufferUsages::STORAGE,
                                mapped_at_creation: false,
                            }));
                        let args_buffer = indirect_args_buffer
                            .as_ref()
                            .expect("indirect_args_buffer must be set");
                        self.queue.write_buffer(args_buffer, 0, args.as_bytes());
                        pass.draw_indexed_indirect(args_buffer, 0);
                    }
                }
            }
        }

        // Make the buffer "used" on all control-flow paths so the `unused_assignments` lint doesn't
        // fire for the `None` initialization paths. (The real reason this exists is to keep the
        // indirect args buffer alive until `pass` is dropped.)
        let _ = &indirect_args_buffer;

        drop(pass);
        self.queue.submit([encoder.finish()]);

        Ok(())
    }

    fn draw_expanded_passthrough_internal(
        &mut self,
        expanded_vertex_buffer: AerogpuHandle,
        kind: DrawKind,
    ) -> Result<()> {
        let ps_handle = self
            .state
            .ps
            .ok_or_else(|| anyhow!("draw_expanded_passthrough without a bound PS"))?;

        let ps = self
            .resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS handle {ps_handle}"))?;
        if ps.stage != ShaderStage::Fragment {
            bail!("shader {ps_handle} is not a pixel/fragment shader");
        }

        // Determine the PS varying locations we must output. This is driven by the `PsIn` struct's
        // declared `@location(N)` members (builtins like `@builtin(position)` are excluded).
        let ps_locations = super::wgsl_link::locations_in_struct(&ps.wgsl, "PsIn")?;
        let signature = PassthroughVertexShaderKey::from_locations(ps_locations.iter().copied());

        // Validate against WebGPU vertex attribute limits. The expanded buffer uses one vertex
        // attribute per vec4, plus one for position.
        let required_vertex_attributes = 1u32 + signature.locations().len() as u32;
        let max_vertex_attributes = self.device.limits().max_vertex_attributes;
        if required_vertex_attributes > max_vertex_attributes {
            bail!(
                "expanded draw requires {required_vertex_attributes} vertex attributes (pos + {} varyings), but device limit max_vertex_attributes={max_vertex_attributes}",
                signature.locations().len()
            );
        }

        // Register the generated passthrough VS into the shared PipelineCache shader-module cache.
        let (vs_hash, _module) = self
            .pipelines
            .get_or_create_passthrough_vertex_shader(&self.device, &signature);

        let expanded_layout = signature.expanded_vertex_layout();
        let vertex_buffer_keys = vec![expanded_layout.key()];

        let (color_attachments, color_target_keys, color_size) =
            build_color_attachments(&self.resources, &self.state)?;

        let (depth_attachment, depth_target_key, depth_state, depth_size) =
            build_depth_attachment(&self.resources, &self.state)?;

        let target_size = color_size
            .or(depth_size)
            .ok_or_else(|| anyhow!("draw without bound render targets"))?;

        let primitive_topology = map_topology(self.state.primitive_topology)?;
        let strip_index_format = match primitive_topology {
            wgpu::PrimitiveTopology::LineStrip | wgpu::PrimitiveTopology::TriangleStrip => {
                self.state.index_buffer.map(|ib| ib.format)
            }
            _ => None,
        };
        let cull_mode = self.state.rasterizer_state.cull_mode;
        let front_face = self.state.rasterizer_state.front_face;
        let scissor_enabled = self.state.rasterizer_state.scissor_enable;

        let pipeline_bindings = reflection_bindings::build_pipeline_bindings_info(
            &self.device,
            &mut self.bind_group_layout_cache,
            [
                reflection_bindings::ShaderBindingSet::Guest(&[] as &[crate::Binding]),
                reflection_bindings::ShaderBindingSet::Guest(ps.reflection.bindings.as_slice()),
            ],
            reflection_bindings::BindGroupIndexValidation::GuestShaders,
        )?;
        let reflection_bindings::PipelineBindingsInfo {
            layout_key,
            group_layouts,
            group_bindings,
        } = pipeline_bindings;

        let pipeline_layout = {
            let device = &self.device;
            let cache = &mut self.pipeline_layout_cache;
            cache.get_or_create_with(&layout_key, || {
                let layout_refs: Vec<&wgpu::BindGroupLayout> =
                    group_layouts.iter().map(|l| l.layout.as_ref()).collect();
                Arc::new(
                    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aero-d3d11 aerogpu expanded pipeline layout"),
                        bind_group_layouts: &layout_refs,
                        push_constant_ranges: &[],
                    }),
                )
            })
        };

        // WebGPU requires that the fragment shader's `@location(N)` outputs line up with the render
        // pipeline's `ColorTargetState` array. D3D discards writes to unbound RTVs instead.
        //
        // To emulate D3D, trim fragment outputs to the set of currently bound render target slots.
        let mut keep_output_locations = BTreeSet::new();
        for (slot, handle) in self.state.render_targets.colors.iter().enumerate() {
            if handle.is_some() {
                keep_output_locations.insert(slot as u32);
            }
        }

        let mut linked_ps_wgsl = std::borrow::Cow::Borrowed(ps.wgsl.as_str());
        let declared_outputs =
            super::wgsl_link::declared_ps_output_locations(linked_ps_wgsl.as_ref())?;
        let missing_outputs: BTreeSet<u32> = declared_outputs
            .difference(&keep_output_locations)
            .copied()
            .collect();
        if !missing_outputs.is_empty() {
            linked_ps_wgsl =
                std::borrow::Cow::Owned(super::wgsl_link::trim_ps_outputs_to_locations(
                    linked_ps_wgsl.as_ref(),
                    &keep_output_locations,
                ));
        }

        let linked_ps_hash = if linked_ps_wgsl.as_ref() == ps.wgsl.as_str() {
            ps.hash
        } else {
            let (hash, _module) = self.pipelines.get_or_create_shader_module(
                &self.device,
                ShaderStage::Fragment,
                linked_ps_wgsl.as_ref(),
                Some("aero-d3d11 aerogpu linked fragment shader"),
            );
            hash
        };

        let key = RenderPipelineKey {
            vertex_shader: vs_hash,
            fragment_shader: linked_ps_hash,
            color_targets: color_target_keys,
            depth_stencil: depth_target_key,
            primitive_topology,
            strip_index_format,
            cull_mode,
            front_face,
            vertex_buffers: vertex_buffer_keys,
            sample_count: 1,
            layout: layout_key,
        };

        let blend = self.state.blend_state;
        let mut color_target_states: Vec<Option<wgpu::ColorTargetState>> = Vec::new();
        for ct in &key.color_targets {
            let Some(ct) = ct else {
                color_target_states.push(None);
                continue;
            };
            color_target_states.push(Some(wgpu::ColorTargetState {
                format: ct.format,
                blend: blend.blend,
                write_mask: ct.write_mask,
            }));
        }

        let depth_stencil_state = depth_state.clone();
        let expanded_layout_for_pipeline = expanded_layout.clone();

        let pipeline = self.pipelines.get_or_create_render_pipeline(
            &self.device,
            key,
            move |device, vs_module, fs_module| {
                let pipeline_layout = pipeline_layout.as_ref();

                let vertex_buffers = [expanded_layout_for_pipeline.as_wgpu()];

                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("aero-d3d11 aerogpu expanded render pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: vs_module,
                        entry_point: "vs_main",
                        buffers: &vertex_buffers,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: fs_module,
                        entry_point: "fs_main",
                        targets: &color_target_states,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: primitive_topology,
                        strip_index_format,
                        front_face,
                        cull_mode,
                        ..Default::default()
                    },
                    depth_stencil: depth_stencil_state,
                    multisample: wgpu::MultisampleState {
                        count: 1,
                        ..Default::default()
                    },
                    multiview: None,
                })
            },
        )?;

        let mut bind_groups: Vec<Arc<wgpu::BindGroup>> = Vec::with_capacity(group_layouts.len());
        for (group_index, (layout, bindings)) in
            group_layouts.iter().zip(group_bindings.iter()).enumerate()
        {
            let stage_state = match group_index as u32 {
                0 => Some(&self.state.bindings.vs),
                1 => Some(&self.state.bindings.ps),
                _ => None,
            };
            let provider = RuntimeBindGroupProvider {
                resources: &self.resources,
                stage_state,
                dummy_uniform: &self.dummy_uniform,
                dummy_storage: &self.dummy_storage,
                dummy_texture_view: &self.dummy_texture_view,
                default_sampler: &self.default_sampler,
            };
            bind_groups.push(reflection_bindings::build_bind_group(
                &self.device,
                &mut self.bind_group_cache,
                layout,
                bindings,
                &provider,
            )?);
        }

        let expanded_vb = self
            .resources
            .buffers
            .get(&expanded_vertex_buffer)
            .ok_or_else(|| anyhow!("unknown expanded vertex buffer {expanded_vertex_buffer}"))?;

        // Encode the draw.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 aerogpu expanded draw encoder"),
            });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aero-d3d11 aerogpu expanded draw pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        pass.set_pipeline(pipeline);
        for (group_index, bind_group) in bind_groups.iter().enumerate() {
            pass.set_bind_group(group_index as u32, bind_group.as_ref(), &[]);
        }

        // Viewport/scissor are dynamic state; apply on every draw.
        let mut skip_draw = false;
        let default_viewport = Viewport {
            x: 0.0,
            y: 0.0,
            width: target_size.0 as f32,
            height: target_size.1 as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let viewport_opt = self.state.viewport;
        let mut viewport = viewport_opt.unwrap_or(default_viewport);
        if !viewport.x.is_finite()
            || !viewport.y.is_finite()
            || !viewport.width.is_finite()
            || !viewport.height.is_finite()
            || !viewport.min_depth.is_finite()
            || !viewport.max_depth.is_finite()
        {
            viewport = default_viewport;
        }

        let max_w = target_size.0 as f32;
        let max_h = target_size.1 as f32;
        let left = viewport.x.max(0.0);
        let top = viewport.y.max(0.0);
        let right = (viewport.x + viewport.width).max(0.0).min(max_w);
        let bottom = (viewport.y + viewport.height).max(0.0).min(max_h);
        let width = (right - left).max(0.0);
        let height = (bottom - top).max(0.0);

        let mut viewport_empty = false;
        if viewport_opt.is_some() && (viewport.width <= 0.0 || viewport.height <= 0.0) {
            viewport_empty = true;
        }
        if !viewport_empty && width > 0.0 && height > 0.0 {
            let mut min_depth = viewport.min_depth.clamp(0.0, 1.0);
            let mut max_depth = viewport.max_depth.clamp(0.0, 1.0);
            if min_depth > max_depth {
                std::mem::swap(&mut min_depth, &mut max_depth);
            }
            pass.set_viewport(left, top, width, height, min_depth, max_depth);
        } else if viewport_opt.is_some() {
            viewport_empty = true;
        }

        let mut scissor_empty = false;
        if scissor_enabled {
            if let Some(scissor) = self.state.scissor {
                let x = scissor.x.min(target_size.0);
                let y = scissor.y.min(target_size.1);
                let width = scissor.width.min(target_size.0.saturating_sub(x));
                let height = scissor.height.min(target_size.1.saturating_sub(y));
                if width > 0 && height > 0 {
                    pass.set_scissor_rect(x, y, width, height);
                } else {
                    scissor_empty = true;
                    pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
                }
            } else {
                pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
            }
        } else {
            pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
        }

        if viewport_empty || scissor_empty {
            skip_draw = true;
        }

        pass.set_vertex_buffer(0, expanded_vb.buffer.slice(..));

        match kind {
            DrawKind::NonIndexed(args) => {
                if !skip_draw {
                    pass.draw(
                        args.first_vertex..args.first_vertex.saturating_add(args.vertex_count),
                        args.first_instance
                            ..args.first_instance.saturating_add(args.instance_count),
                    );
                }
            }
            DrawKind::Indexed(_) => {
                bail!("draw_expanded_passthrough does not support indexed draws yet");
            }
        }

        drop(pass);
        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    pub async fn read_texture_rgba8(&self, handle: AerogpuHandle) -> Result<Vec<u8>> {
        let tex = self
            .resources
            .textures
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown texture {handle}"))?;
        let needs_bgra_swizzle = match tex.desc.format {
            wgpu::TextureFormat::Rgba8Unorm => false,
            wgpu::TextureFormat::Bgra8Unorm => true,
            other => {
                bail!("read_texture_rgba8 only supports Rgba8Unorm/Bgra8Unorm (got {other:?})")
            }
        };

        let width = tex.desc.width;
        let height = tex.desc.height;

        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu readback staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 aerogpu readback encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).ok();
        });
        self.poll_wait();
        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
            .context("wgpu: map_async failed")?;

        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in 0..height as usize {
            let start = row * padded_bytes_per_row as usize;
            out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        staging.unmap();

        if needs_bgra_swizzle {
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }

        Ok(out)
    }
}

#[derive(Debug, Copy, Clone)]
enum DrawKind {
    NonIndexed(DrawIndirectArgs),
    Indexed(DrawIndexedIndirectArgs),
}

struct RuntimeBindGroupProvider<'a> {
    resources: &'a AerogpuResources,
    stage_state: Option<&'a super::aerogpu_state::StageBindings>,
    dummy_uniform: &'a wgpu::Buffer,
    dummy_storage: &'a wgpu::Buffer,
    dummy_texture_view: &'a wgpu::TextureView,
    default_sampler: &'a aero_gpu::bindings::samplers::CachedSampler,
}

impl reflection_bindings::BindGroupResourceProvider for RuntimeBindGroupProvider<'_> {
    fn constant_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        let stage = self.stage_state?;
        let handle = stage
            .constant_buffers
            .get(slot as usize)
            .copied()
            .flatten()?;
        let buf = self.resources.buffers.get(&handle)?;
        Some(reflection_bindings::BufferBinding {
            id: BufferId(handle as u64),
            buffer: &buf.buffer,
            offset: 0,
            size: None,
            total_size: buf.size,
        })
    }

    fn constant_buffer_scratch(&self, _slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
        None
    }

    fn texture2d(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
        let stage = self.stage_state?;
        let handle = stage.textures.get(slot as usize).copied().flatten()?;
        let tex = self.resources.textures.get(&handle)?;
        Some((TextureViewId(handle as u64), &tex.view))
    }

    fn srv_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        let stage = self.stage_state?;
        let handle = stage.textures.get(slot as usize).copied().flatten()?;
        let buf = self.resources.buffers.get(&handle)?;
        Some(reflection_bindings::BufferBinding {
            id: BufferId(handle as u64),
            buffer: &buf.buffer,
            offset: 0,
            size: None,
            total_size: buf.size,
        })
    }

    fn sampler(&self, slot: u32) -> Option<&aero_gpu::bindings::samplers::CachedSampler> {
        let stage = self.stage_state?;
        let handle = stage.samplers.get(slot as usize).copied().flatten()?;
        self.resources.samplers.get(&handle)
    }

    fn uav_buffer(&self, _slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        // The aerogpu test runtime does not currently model buffer UAV bindings.
        None
    }

    fn dummy_uniform(&self) -> &wgpu::Buffer {
        self.dummy_uniform
    }

    fn dummy_storage(&self) -> &wgpu::Buffer {
        self.dummy_storage
    }

    fn dummy_texture_view(&self) -> &wgpu::TextureView {
        self.dummy_texture_view
    }

    fn default_sampler(&self) -> &aero_gpu::bindings::samplers::CachedSampler {
        self.default_sampler
    }
}

fn validate_sm5_gs_streams(program: &Sm4Program) -> Result<()> {
    // DXBC encodes SM4/SM5 shaders as a stream of DWORD tokens. We only need to recognize the
    // `emit_stream` / `cut_stream` instruction forms, which carry the stream index as an
    // immediate32 operand.
    //
    // Keep this scan token-level (rather than using the full decoder) so we can detect unsupported
    // multi-stream usage even when other parts of the shader are not yet decodable/translateable.
    let declared_len = program.tokens.get(1).copied().unwrap_or(0) as usize;
    if declared_len < 2 || declared_len > program.tokens.len() {
        // Malformed token stream; the real decoder will report a better error later.
        return Ok(());
    }

    let toks = &program.tokens[..declared_len];
    let mut i = 2usize;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & sm4_opcode::OPCODE_MASK;
        let len =
            ((opcode_token >> sm4_opcode::OPCODE_LEN_SHIFT) & sm4_opcode::OPCODE_LEN_MASK) as usize;
        if len == 0 || i + len > toks.len() {
            // Malformed instruction; let downstream decode/translation surface the issue.
            return Ok(());
        }

        let stream_opcode_name = if opcode == sm4_opcode::OPCODE_EMIT_STREAM {
            Some("emit_stream")
        } else if opcode == sm4_opcode::OPCODE_CUT_STREAM {
            Some("cut_stream")
        } else {
            None
        };

        if let Some(op_name) = stream_opcode_name {
            // `emit_stream` / `cut_stream` take exactly one operand: an immediate32 scalar
            // (replicated lanes) indicating the stream index.
            //
            // Skip any extended opcode tokens to find the operand token.
            let inst_end = i + len;
            let mut operand_pos = i + 1;
            let mut extended = (opcode_token & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            while extended {
                if operand_pos >= inst_end {
                    return Ok(());
                }
                let Some(ext) = toks.get(operand_pos).copied() else {
                    return Ok(());
                };
                operand_pos += 1;
                extended = (ext & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            }

            if operand_pos >= inst_end {
                return Ok(());
            }
            let Some(operand_token) = toks.get(operand_pos).copied() else {
                return Ok(());
            };
            operand_pos += 1;

            let ty =
                (operand_token >> sm4_opcode::OPERAND_TYPE_SHIFT) & sm4_opcode::OPERAND_TYPE_MASK;
            if ty != sm4_opcode::OPERAND_TYPE_IMMEDIATE32 {
                // Malformed stream operand; the decoder will surface a better error later.
                return Ok(());
            }

            // Skip extended operand tokens (modifiers).
            let mut operand_ext = (operand_token & sm4_opcode::OPERAND_EXTENDED_BIT) != 0;
            while operand_ext {
                if operand_pos >= inst_end {
                    return Ok(());
                }
                let Some(ext) = toks.get(operand_pos).copied() else {
                    return Ok(());
                };
                operand_pos += 1;
                operand_ext = (ext & sm4_opcode::OPERAND_EXTENDED_BIT) != 0;
            }

            // Immediate operands should have no indices, but if they do, bail out and let the real
            // decoder handle it.
            let index_dim = (operand_token >> sm4_opcode::OPERAND_INDEX_DIMENSION_SHIFT)
                & sm4_opcode::OPERAND_INDEX_DIMENSION_MASK;
            if index_dim != sm4_opcode::OPERAND_INDEX_DIMENSION_0D {
                return Ok(());
            }

            let num_components = operand_token & sm4_opcode::OPERAND_NUM_COMPONENTS_MASK;
            let stream = match num_components {
                // Scalar immediate (1 DWORD payload).
                1 => {
                    if operand_pos >= inst_end {
                        return Ok(());
                    }
                    toks[operand_pos]
                }
                // 4-component immediate (4 DWORD payload); `decode_stream_index` uses lane 0.
                2 => {
                    if operand_pos + 3 >= inst_end {
                        return Ok(());
                    }
                    toks[operand_pos]
                }
                _ => return Ok(()),
            };

            if stream != 0 {
                bail!(
                    "create_shader_dxbc: unsupported {op_name} stream index {stream} (only stream 0 is supported)"
                );
            }
        }

        i += len;
    }

    Ok(())
}

#[derive(Debug)]
struct BuiltVertexState {
    layouts: Vec<VertexBufferLayoutOwned>,
    keys: Vec<VertexBufferLayoutKey>,
    /// WebGPU vertex buffer slot → D3D11 input slot.
    wgpu_slot_to_d3d_slot: Vec<u32>,
}

fn build_vertex_state(
    resources: &AerogpuResources,
    state: &D3D11ShadowState,
    vs_signature: &[VsInputSignatureElement],
) -> Result<BuiltVertexState> {
    let Some(layout_handle) = state.input_layout else {
        return Ok(BuiltVertexState {
            layouts: Vec::new(),
            keys: Vec::new(),
            wgpu_slot_to_d3d_slot: Vec::new(),
        });
    };

    let layout = resources
        .input_layouts
        .get(&layout_handle)
        .ok_or_else(|| anyhow!("unknown input layout handle {layout_handle}"))?;

    let mut slot_strides = vec![0u32; MAX_INPUT_SLOTS as usize];
    for (slot, binding) in state
        .vertex_buffers
        .iter()
        .enumerate()
        .take(slot_strides.len())
    {
        if let Some(binding) = binding {
            slot_strides[slot] = binding.stride;
        }
    }

    let fallback_signature;
    let sig = if vs_signature.is_empty() {
        fallback_signature = build_fallback_vs_signature(&layout.layout);
        fallback_signature.as_slice()
    } else {
        vs_signature
    };

    let binding = InputLayoutBinding::new(&layout.layout, &slot_strides);
    let mapped = map_layout_to_shader_locations_compact(&binding, sig)
        .map_err(|e| anyhow!("failed to map input layout to shader locations: {e}"))?;

    let keys = mapped
        .buffers
        .iter()
        .map(|l| VertexBufferLayoutKey {
            array_stride: l.array_stride,
            step_mode: l.step_mode,
            attributes: l
                .attributes
                .iter()
                .copied()
                .map(VertexAttributeKey::from)
                .collect(),
        })
        .collect();

    let mut wgpu_slot_to_d3d_slot = vec![0u32; mapped.buffers.len()];
    for (d3d_slot, wgpu_slot) in &mapped.d3d_slot_to_wgpu_slot {
        wgpu_slot_to_d3d_slot[*wgpu_slot as usize] = *d3d_slot;
    }

    Ok(BuiltVertexState {
        layouts: mapped.buffers,
        keys,
        wgpu_slot_to_d3d_slot,
    })
}

type ColorAttachments<'a> = (
    Vec<Option<wgpu::RenderPassColorAttachment<'a>>>,
    Vec<Option<ColorTargetKey>>,
    Option<(u32, u32)>,
);

fn build_color_attachments<'a>(
    resources: &'a AerogpuResources,
    state: &D3D11ShadowState,
) -> Result<ColorAttachments<'a>> {
    // WebGPU requires the render pass color attachments slice length to match the pipeline's
    // `FragmentState.targets` length. Preserve the D3D11 slot indices by including gaps, but trim
    // trailing `None` entries so we don't force a fixed length of 8.
    let mut last_slot: Option<usize> = None;
    for (slot, handle) in state.render_targets.colors.iter().enumerate() {
        if handle.is_some() {
            last_slot = Some(slot);
        }
    }
    let len = last_slot.map(|v| v + 1).unwrap_or(0);

    let mut attachments = Vec::with_capacity(len);
    let mut keys = Vec::with_capacity(len);

    let mut size: Option<(u32, u32)> = None;
    for handle in state.render_targets.colors.iter().take(len) {
        let Some(handle) = handle else {
            attachments.push(None);
            keys.push(None);
            continue;
        };

        let tex = resources
            .textures
            .get(handle)
            .ok_or_else(|| anyhow!("unknown render target texture {handle}"))?;

        let this_size = (tex.desc.width, tex.desc.height);
        if let Some(expected) = size {
            if expected != this_size {
                bail!(
                    "mismatched render target sizes: {:?} vs {:?}",
                    expected,
                    this_size
                );
            }
        } else {
            size = Some(this_size);
        }

        attachments.push(Some(wgpu::RenderPassColorAttachment {
            view: &tex.view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        }));

        keys.push(Some(ColorTargetKey {
            format: tex.desc.format,
            blend: state.blend_state.blend.map(Into::into),
            write_mask: state.blend_state.write_mask,
        }));
    }

    Ok((attachments, keys, size))
}

fn build_depth_attachment<'a>(
    resources: &'a AerogpuResources,
    state: &D3D11ShadowState,
) -> Result<(
    Option<wgpu::RenderPassDepthStencilAttachment<'a>>,
    Option<aero_gpu::pipeline_key::DepthStencilKey>,
    Option<wgpu::DepthStencilState>,
    Option<(u32, u32)>,
)> {
    let Some(depth_handle) = state.render_targets.depth_stencil else {
        return Ok((None, None, None, None));
    };

    let tex = resources
        .textures
        .get(&depth_handle)
        .ok_or_else(|| anyhow!("unknown depth-stencil texture {depth_handle}"))?;

    let DepthStencilState {
        depth_enable,
        depth_write_enable,
        depth_compare,
        stencil_enable,
        stencil_read_mask,
        stencil_write_mask,
        depth_bias,
    } = state.depth_stencil_state;

    let depth_compare = if depth_enable {
        depth_compare
    } else {
        wgpu::CompareFunction::Always
    };
    let depth_write_enabled = depth_enable && depth_write_enable;

    let _ = stencil_enable;
    // P0: the protocol doesn't carry full stencil ops/functions yet.
    let stencil_state = wgpu::StencilState {
        front: wgpu::StencilFaceState::IGNORE,
        back: wgpu::StencilFaceState::IGNORE,
        read_mask: stencil_read_mask as u32,
        write_mask: stencil_write_mask as u32,
    };

    let depth_stencil_state = wgpu::DepthStencilState {
        format: tex.desc.format,
        depth_write_enabled,
        depth_compare,
        stencil: stencil_state,
        bias: wgpu::DepthBiasState {
            constant: depth_bias,
            slope_scale: 0.0,
            clamp: 0.0,
        },
    };

    let attachment = wgpu::RenderPassDepthStencilAttachment {
        view: &tex.view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0),
            store: wgpu::StoreOp::Store,
        }),
    };

    Ok((
        Some(attachment),
        Some(depth_stencil_state.clone().into()),
        Some(depth_stencil_state),
        Some((tex.desc.width, tex.desc.height)),
    ))
}

fn map_topology(topology: PrimitiveTopology) -> Result<wgpu::PrimitiveTopology> {
    Ok(match topology {
        PrimitiveTopology::PointList => wgpu::PrimitiveTopology::PointList,
        PrimitiveTopology::LineList => wgpu::PrimitiveTopology::LineList,
        PrimitiveTopology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
        PrimitiveTopology::TriangleList => wgpu::PrimitiveTopology::TriangleList,
        PrimitiveTopology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        PrimitiveTopology::TriangleFan => bail!("TriangleFan is not supported by WebGPU"),
    })
}

fn extract_vs_input_signature_unique_locations(
    signatures: &crate::ShaderSignatures,
    module: &crate::Sm4Module,
) -> Result<Vec<VsInputSignatureElement>> {
    const D3D_NAME_VERTEX_ID: u32 = 6;
    const D3D_NAME_INSTANCE_ID: u32 = 8;

    let Some(isgn) = signatures.isgn.as_ref() else {
        return Ok(Vec::new());
    };

    let mut sivs = HashMap::<u32, u32>::new();
    for decl in &module.decls {
        if let crate::Sm4Decl::InputSiv { reg, sys_value, .. } = decl {
            sivs.insert(*reg, *sys_value);
        }
    }

    let mut out = Vec::new();
    let mut next_location = 0u32;

    for p in &isgn.parameters {
        let sys_value = sivs
            .get(&p.register)
            .copied()
            .or_else(|| (p.system_value_type != 0).then_some(p.system_value_type));

        let is_builtin = matches!(sys_value, Some(D3D_NAME_VERTEX_ID | D3D_NAME_INSTANCE_ID))
            || p.semantic_name.eq_ignore_ascii_case("SV_VertexID")
            || p.semantic_name.eq_ignore_ascii_case("SV_InstanceID");
        if is_builtin {
            continue;
        }

        out.push(VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
            mask: p.mask,
            shader_location: next_location,
        });
        next_location += 1;
    }

    Ok(out)
}

fn extract_vs_input_signature(
    signatures: &crate::ShaderSignatures,
) -> Result<Vec<VsInputSignatureElement>> {
    let Some(isgn) = signatures.isgn.as_ref() else {
        return Ok(Vec::new());
    };

    // D3D semantics are case-insensitive, but the signature chunk stores the original string. The
    // aerogpu ILAY protocol only preserves a hash, so we canonicalize to ASCII uppercase to match
    // how the guest typically hashes semantic names.
    Ok(isgn
        .parameters
        .iter()
        .map(|p| VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
            mask: p.mask,
            shader_location: p.register,
        })
        .collect())
}

fn build_fallback_vs_signature(layout: &InputLayoutDesc) -> Vec<VsInputSignatureElement> {
    let mut seen: HashMap<(u32, u32), u32> = HashMap::new();
    let mut out: Vec<VsInputSignatureElement> = Vec::new();

    for elem in &layout.elements {
        let key = (elem.semantic_name_hash, elem.semantic_index);
        if seen.contains_key(&key) {
            continue;
        }
        let reg = out.len() as u32;
        seen.insert(key, reg);
        out.push(VsInputSignatureElement {
            semantic_name_hash: key.0,
            semantic_index: key.1,
            input_register: reg,
            mask: 0xF,
            shader_location: reg,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_gpu::pipeline_key::{ComputePipelineKey, PipelineLayoutKey};
    use aero_gpu::GpuError;

    fn require_webgpu() -> bool {
        let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
            return false;
        };
        let v = raw.trim();
        v == "1"
            || v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
    }

    fn skip_or_panic(test_name: &str, reason: &str) {
        if require_webgpu() {
            panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
        }
        eprintln!("skipping {test_name}: {reason}");
    }

    #[test]
    fn pipeline_cache_compute_support_is_derived_from_downlevel_flags() {
        pollster::block_on(async {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                    .ok()
                    .map(|v| v.is_empty())
                    .unwrap_or(true);
                if needs_runtime_dir {
                    let dir = std::env::temp_dir().join(format!(
                        "aero-d3d11-aerogpu-xdg-runtime-{}",
                        std::process::id()
                    ));
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                    std::env::set_var("XDG_RUNTIME_DIR", &dir);
                }
            }

            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
                backends: if cfg!(target_os = "linux") {
                    wgpu::Backends::GL
                } else {
                    // Prefer "native" backends; this avoids noisy platform warnings from
                    // initializing GL/WAYLAND stacks in headless CI environments.
                    wgpu::Backends::PRIMARY
                },
                ..Default::default()
            });

            let adapter = match instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                })
                .await
            {
                Some(adapter) => Some(adapter),
                None => {
                    instance
                        .request_adapter(&wgpu::RequestAdapterOptions {
                            power_preference: wgpu::PowerPreference::LowPower,
                            compatible_surface: None,
                            force_fallback_adapter: false,
                        })
                        .await
                }
            };
            let Some(adapter) = adapter else {
                skip_or_panic(
                    module_path!(),
                    "wgpu unavailable (no suitable adapter found)",
                );
                return;
            };

            let expected_supports_compute = adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);

            let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
                Ok(rt) => rt,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // `GpuCapabilities::from_device` cannot determine compute support from a `wgpu::Device`
            // alone (notably WebGL2 has no compute stage), so it defaults `supports_compute=false`.
            // `new_for_tests` must override this using adapter downlevel flags so that
            // `PipelineCache` enables/disables compute pipelines deterministically across backends.
            let shader_hash = 0x1234_u128;
            let key = ComputePipelineKey {
                shader: shader_hash,
                layout: PipelineLayoutKey::empty(),
            };
            let err = rt
                .pipelines
                .get_or_create_compute_pipeline(&rt.device, key, |_device, _cs| unreachable!())
                .unwrap_err();

            match (expected_supports_compute, err) {
                (false, GpuError::Unsupported("compute")) => {}
                (
                    true,
                    GpuError::MissingShaderModule {
                        stage: ShaderStage::Compute,
                        hash,
                    },
                ) if hash == shader_hash => {}
                (expected, err) => panic!(
                    "expected supports_compute={expected} based on DownlevelFlags, got error {err:?}"
                ),
            }
        });
    }

    #[test]
    fn trims_fragment_outputs_when_mrt_is_partially_bound() {
        // Regression test for D3D-style MRT behavior:
        // - Fragment shader can declare multiple `@location(N)` outputs.
        // - The app can bind fewer render targets than the shader declares.
        // - Writes to unbound targets are discarded (shader outputs must be trimmed for WebGPU).
        pollster::block_on(async {
            let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
                Ok(rt) => rt,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const W: u32 = 4;
            const H: u32 = 4;
            const RT0: AerogpuHandle = 1;
            const RT2: AerogpuHandle = 2;

            let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
            rt.create_texture2d(RT0, W, H, wgpu::TextureFormat::Rgba8Unorm, usage);
            rt.create_texture2d(RT2, W, H, wgpu::TextureFormat::Rgba8Unorm, usage);

            let vs_wgsl = r#"
                @vertex
                fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
                    var pos = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -1.0),
                        vec2<f32>( 3.0, -1.0),
                        vec2<f32>(-1.0,  3.0),
                    );
                    let p = pos[index];
                    return vec4<f32>(p, 0.0, 1.0);
                }
            "#;

            // Fragment shader writes to `@location(0)` and `@location(2)`.
            let fs_wgsl = r#"
                struct PsOut {
                    @location(0) o0: vec4<f32>,
                    @location(2) o2: vec4<f32>,
                };

                @fragment
                fn fs_main() -> PsOut {
                    var out: PsOut;
                    out.o0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);
                    out.o2 = vec4<f32>(0.0, 1.0, 0.0, 1.0);
                    return out;
                }
            "#;

            let vs_module = rt
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("wgsl_link mrt trim vs"),
                    source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
                });
            let fs_module = rt
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("wgsl_link mrt trim fs"),
                    source: wgpu::ShaderSource::Wgsl(fs_wgsl.into()),
                });

            let layout = rt
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("wgsl_link mrt trim layout"),
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });

            let ct = wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            };

            // (1) Only RT0 bound: untrimmed shader should fail pipeline creation, trimmed should succeed.
            let targets_rt0 = [Some(ct.clone())];
            rt.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let _ = rt
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("wgsl_link mrt untrimmed rt0-only"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &vs_module,
                        entry_point: "vs_main",
                        buffers: &[],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &fs_module,
                        entry_point: "fs_main",
                        targets: &targets_rt0,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                });
            rt.device.poll(wgpu::Maintain::Wait);
            let err = rt.device.pop_error_scope().await;
            assert!(
                err.is_some(),
                "expected pipeline creation to fail when RT2 is unbound but shader writes @location(2)"
            );

            // Now go through the AeroGPU runtime path: the runtime should trim `@location(2)` at
            // pipeline-creation time and the draw should succeed with only RT0 bound.
            const VS: AerogpuHandle = 10;
            const PS: AerogpuHandle = 11;
            let (vs_hash, _vs_module_cached) = rt.pipelines.get_or_create_shader_module(
                &rt.device,
                ShaderStage::Vertex,
                vs_wgsl,
                Some("wgsl_link mrt trim runtime vs"),
            );
            let (ps_hash, _ps_module_cached) = rt.pipelines.get_or_create_shader_module(
                &rt.device,
                ShaderStage::Fragment,
                fs_wgsl,
                Some("wgsl_link mrt trim runtime fs"),
            );
            rt.resources.shaders.insert(
                VS,
                ShaderResource {
                    stage: ShaderStage::Vertex,
                    wgsl: vs_wgsl.to_owned(),
                    hash: vs_hash,
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                },
            );
            rt.resources.shaders.insert(
                PS,
                ShaderResource {
                    stage: ShaderStage::Fragment,
                    wgsl: fs_wgsl.to_owned(),
                    hash: ps_hash,
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                },
            );
            rt.bind_shaders(Some(VS), None, Some(PS));

            let mut colors = [None; 8];
            colors[0] = Some(RT0);
            rt.set_render_targets(&colors, None);
            rt.set_primitive_topology(PrimitiveTopology::TriangleList);
            rt.draw(3, 1, 0, 0).expect("runtime draw");

            let bytes = rt.read_texture_rgba8(RT0).await.expect("read RT0");
            assert_eq!(&bytes[..4], &[255, 0, 0, 255], "RT0 must be red");

            // (2) RT0 + RT2 bound with a gap at RT1: pipeline creation succeeds and output2 preserved.
            let targets_gap = [Some(ct.clone()), None, Some(ct)];
            rt.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let pipeline_gap = rt
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("wgsl_link mrt untrimmed rt0+rt2"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &vs_module,
                        entry_point: "vs_main",
                        buffers: &[],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &fs_module,
                        entry_point: "fs_main",
                        targets: &targets_gap,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                });
            rt.device.poll(wgpu::Maintain::Wait);
            let err = rt.device.pop_error_scope().await;
            assert!(
                err.is_none(),
                "untrimmed pipeline must succeed when RT2 is bound"
            );

            let view0 = &rt.resources.textures.get(&RT0).expect("RT0 created").view;
            let view2 = &rt.resources.textures.get(&RT2).expect("RT2 created").view;

            let mut encoder = rt
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("wgsl_link mrt trim encoder rt0+rt2"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("wgsl_link mrt trim pass rt0+rt2"),
                    color_attachments: &[
                        Some(wgpu::RenderPassColorAttachment {
                            view: view0,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                        None,
                        Some(wgpu::RenderPassColorAttachment {
                            view: view2,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                    ],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&pipeline_gap);
                pass.draw(0..3, 0..1);
            }
            rt.queue.submit([encoder.finish()]);

            let bytes0 = rt.read_texture_rgba8(RT0).await.expect("read RT0");
            let bytes2 = rt.read_texture_rgba8(RT2).await.expect("read RT2");
            assert_eq!(&bytes0[..4], &[255, 0, 0, 255], "RT0 must be red");
            assert_eq!(&bytes2[..4], &[0, 255, 0, 255], "RT2 must be green");
        });
    }
}
