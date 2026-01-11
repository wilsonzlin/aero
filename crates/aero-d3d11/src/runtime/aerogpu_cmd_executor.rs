use std::collections::HashMap;
use std::num::NonZeroU64;
use std::ops::Range;
use std::sync::Arc;

use aero_gpu::bindings::layout_cache::{BindGroupLayoutCache, CachedBindGroupLayout};
use aero_gpu::guest_memory::{GuestMemory, GuestMemoryError};
use aero_gpu::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use aero_gpu::pipeline_key::{ColorTargetKey, PipelineLayoutKey, RenderPipelineKey, ShaderHash};
use aero_gpu::GpuCapabilities;
use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_copy_buffer_le, decode_cmd_copy_texture2d_le,
    decode_cmd_create_input_layout_blob_le, decode_cmd_create_shader_dxbc_payload_le,
    decode_cmd_set_vertex_buffers_bindings_le, decode_cmd_upload_resource_payload_le,
    AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AerogpuSamplerAddressMode,
    AerogpuSamplerFilter, AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_CLEAR_STENCIL,
    AEROGPU_COPY_FLAG_WRITEBACK_DST, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_SCANOUT,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AEROGPU_ALLOC_FLAG_READONLY};
use anyhow::{anyhow, bail, Context, Result};

use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    VertexBufferLayoutOwned, VsInputSignatureElement, MAX_INPUT_SLOTS,
};
use crate::{
    parse_signatures, translate_sm4_module_to_wgsl, Binding, BindingKind, DxbcFile,
    ShaderReflection, ShaderTranslation, Sm4Program,
};

const DEFAULT_MAX_VERTEX_SLOTS: usize = MAX_INPUT_SLOTS as usize;
const DEFAULT_MAX_TEXTURE_SLOTS: usize = 16;
const DEFAULT_MAX_SAMPLER_SLOTS: usize = 16;
const DEFAULT_MAX_CONSTANT_BUFFER_SLOTS: usize = 16;

// Opcode constants from `aerogpu_cmd.h` (via the canonical `aero-protocol` enum).
const OPCODE_NOP: u32 = AerogpuCmdOpcode::Nop as u32;
const OPCODE_DEBUG_MARKER: u32 = AerogpuCmdOpcode::DebugMarker as u32;

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_DESTROY_RESOURCE: u32 = AerogpuCmdOpcode::DestroyResource as u32;
const OPCODE_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
const OPCODE_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
const OPCODE_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;
const OPCODE_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

const OPCODE_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
const OPCODE_DESTROY_SHADER: u32 = AerogpuCmdOpcode::DestroyShader as u32;
const OPCODE_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;

const OPCODE_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
const OPCODE_DESTROY_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::DestroyInputLayout as u32;
const OPCODE_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;

const OPCODE_SET_BLEND_STATE: u32 = AerogpuCmdOpcode::SetBlendState as u32;
const OPCODE_SET_DEPTH_STENCIL_STATE: u32 = AerogpuCmdOpcode::SetDepthStencilState as u32;
const OPCODE_SET_RASTERIZER_STATE: u32 = AerogpuCmdOpcode::SetRasterizerState as u32;

const OPCODE_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OPCODE_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
const OPCODE_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;

const OPCODE_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
const OPCODE_SET_INDEX_BUFFER: u32 = AerogpuCmdOpcode::SetIndexBuffer as u32;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;

const OPCODE_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
const OPCODE_SET_SAMPLER_STATE: u32 = AerogpuCmdOpcode::SetSamplerState as u32;
const OPCODE_CREATE_SAMPLER: u32 = AerogpuCmdOpcode::CreateSampler as u32;
const OPCODE_DESTROY_SAMPLER: u32 = AerogpuCmdOpcode::DestroySampler as u32;
const OPCODE_SET_SAMPLERS: u32 = AerogpuCmdOpcode::SetSamplers as u32;
const OPCODE_SET_CONSTANT_BUFFERS: u32 = AerogpuCmdOpcode::SetConstantBuffers as u32;

const OPCODE_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPCODE_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
const OPCODE_DRAW_INDEXED: u32 = AerogpuCmdOpcode::DrawIndexed as u32;

const OPCODE_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;
const OPCODE_PRESENT_EX: u32 = AerogpuCmdOpcode::PresentEx as u32;

const OPCODE_FLUSH: u32 = AerogpuCmdOpcode::Flush as u32;

#[derive(Debug, Clone, Default)]
pub struct ExecuteReport {
    pub commands: u32,
    pub unknown_opcodes: u32,
    pub presents: Vec<PresentEvent>,
}

#[derive(Debug, Clone)]
pub struct PresentEvent {
    pub scanout_id: u32,
    pub flags: u32,
    pub d3d9_present_flags: Option<u32>,
    pub presented_render_target: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Viewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    min_depth: f32,
    max_depth: f32,
}

#[derive(Debug, Clone, Copy)]
struct Scissor {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy)]
struct ResourceBacking {
    alloc_id: u32,
    offset_bytes: u64,
}

#[derive(Debug)]
struct BufferResource {
    buffer: wgpu::Buffer,
    size: u64,
    gpu_size: u64,
    backing: Option<ResourceBacking>,
    dirty: Option<Range<u64>>,
}

impl BufferResource {
    fn mark_dirty(&mut self, range: Range<u64>) {
        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        debug_assert!(alignment.is_power_of_two());

        let start = range.start.min(self.size);
        let end = range.end.min(self.size);
        if start >= end {
            return;
        }

        let start = start & !(alignment - 1);
        let end = end.saturating_add(alignment - 1) & !(alignment - 1);
        let end = end.min(self.size);
        if start >= end {
            return;
        }

        let range = start..end;
        self.dirty = Some(match self.dirty.take() {
            Some(existing) => existing.start.min(range.start)..existing.end.max(range.end),
            None => range,
        });
    }
}

#[derive(Debug, Clone, Copy)]
struct Texture2dDesc {
    width: u32,
    height: u32,
    mip_level_count: u32,
    array_layers: u32,
    format: wgpu::TextureFormat,
}

#[derive(Debug)]
struct Texture2dResource {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    desc: Texture2dDesc,
    backing: Option<ResourceBacking>,
    row_pitch_bytes: u32,
    dirty: bool,
    /// CPU shadow for textures updated via `UPLOAD_RESOURCE`.
    ///
    /// The command stream expresses uploads as a linear byte range, but WebGPU uploads are 2D. For
    /// partial updates we patch into this shadow buffer and then re-upload the full texture.
    ///
    /// The shadow is invalidated when the texture is written by GPU operations (draw/clear/copy).
    host_shadow: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct AerogpuSamplerDesc {
    filter: u32,
    address_u: u32,
    address_v: u32,
    address_w: u32,
}

#[derive(Debug)]
struct SamplerResource {
    sampler: wgpu::Sampler,
    #[cfg(debug_assertions)]
    #[allow(dead_code)]
    desc: AerogpuSamplerDesc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShaderStage {
    Vertex,
    Pixel,
    Compute,
}

#[derive(Debug, Clone)]
struct ShaderResource {
    stage: ShaderStage,
    wgsl_hash: ShaderHash,
    dxbc_hash_fnv1a64: u64,
    entry_point: &'static str,
    vs_input_signature: Vec<VsInputSignatureElement>,
    reflection: ShaderReflection,
    #[cfg(debug_assertions)]
    #[allow(dead_code)]
    wgsl_source: String,
}

#[derive(Debug, Clone, Copy)]
struct ConstantBufferBinding {
    buffer: u32,
    offset_bytes: u32,
    size_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct VertexBufferBinding {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct IndexBufferBinding {
    buffer: u32,
    format: wgpu::IndexFormat,
    offset_bytes: u64,
}

#[derive(Debug, Clone)]
struct InputLayoutResource {
    layout: InputLayoutDesc,
    mapping_cache: HashMap<u64, BuiltVertexState>,
}

#[derive(Debug)]
struct AerogpuD3d11Resources {
    buffers: HashMap<u32, BufferResource>,
    textures: HashMap<u32, Texture2dResource>,
    samplers: HashMap<u32, SamplerResource>,
    shaders: HashMap<u32, ShaderResource>,
    input_layouts: HashMap<u32, InputLayoutResource>,
}

impl Default for AerogpuD3d11Resources {
    fn default() -> Self {
        Self {
            buffers: HashMap::new(),
            textures: HashMap::new(),
            samplers: HashMap::new(),
            shaders: HashMap::new(),
            input_layouts: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct AerogpuD3d11State {
    render_targets: Vec<u32>,
    depth_stencil: Option<u32>,
    viewport: Option<Viewport>,
    scissor: Option<Scissor>,

    vertex_buffers: Vec<Option<VertexBufferBinding>>,
    index_buffer: Option<IndexBufferBinding>,
    primitive_topology: wgpu::PrimitiveTopology,

    vs: Option<u32>,
    ps: Option<u32>,
    cs: Option<u32>,
    input_layout: Option<u32>,

    textures_vs: Vec<Option<u32>>,
    textures_ps: Vec<Option<u32>>,
    textures_cs: Vec<Option<u32>>,

    samplers_vs: Vec<Option<u32>>,
    samplers_ps: Vec<Option<u32>>,
    samplers_cs: Vec<Option<u32>>,

    constant_buffers_vs: Vec<Option<ConstantBufferBinding>>,
    constant_buffers_ps: Vec<Option<ConstantBufferBinding>>,
    constant_buffers_cs: Vec<Option<ConstantBufferBinding>>,

    // A small subset of pipeline state. Unsupported values are tolerated and
    // mapped onto sensible defaults.
    blend: Option<wgpu::BlendState>,
    color_write_mask: wgpu::ColorWrites,
    blend_constant: [f32; 4],
    sample_mask: u32,
    depth_enable: bool,
    depth_write_enable: bool,
    depth_compare: wgpu::CompareFunction,
    stencil_enable: bool,
    stencil_read_mask: u8,
    stencil_write_mask: u8,
    cull_mode: Option<wgpu::Face>,
    front_face: wgpu::FrontFace,
    scissor_enable: bool,
    depth_bias: i32,
}

impl Default for AerogpuD3d11State {
    fn default() -> Self {
        Self {
            render_targets: Vec::new(),
            depth_stencil: None,
            viewport: None,
            scissor: None,
            vertex_buffers: vec![None; DEFAULT_MAX_VERTEX_SLOTS],
            index_buffer: None,
            primitive_topology: wgpu::PrimitiveTopology::TriangleList,
            vs: None,
            ps: None,
            cs: None,
            input_layout: None,
            textures_vs: vec![None; DEFAULT_MAX_TEXTURE_SLOTS],
            textures_ps: vec![None; DEFAULT_MAX_TEXTURE_SLOTS],
            textures_cs: vec![None; DEFAULT_MAX_TEXTURE_SLOTS],
            samplers_vs: vec![None; DEFAULT_MAX_SAMPLER_SLOTS],
            samplers_ps: vec![None; DEFAULT_MAX_SAMPLER_SLOTS],
            samplers_cs: vec![None; DEFAULT_MAX_SAMPLER_SLOTS],
            constant_buffers_vs: vec![None; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS],
            constant_buffers_ps: vec![None; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS],
            constant_buffers_cs: vec![None; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS],
            blend: None,
            color_write_mask: wgpu::ColorWrites::ALL,
            blend_constant: [0.0; 4],
            sample_mask: 0xFFFF_FFFF,
            depth_enable: true,
            depth_write_enable: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil_enable: false,
            stencil_read_mask: 0xFF,
            stencil_write_mask: 0xFF,
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
            depth_bias: 0,
        }
    }
}

pub struct AerogpuD3d11Executor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    resources: AerogpuD3d11Resources,
    state: AerogpuD3d11State,

    bind_group_layout_cache: BindGroupLayoutCache,
    pipeline_layout_cache: HashMap<PipelineLayoutKey, Arc<wgpu::PipelineLayout>>,
    fallback_texture_view: wgpu::TextureView,
    fallback_sampler: wgpu::Sampler,
    fallback_uniform_buffer: wgpu::Buffer,
    pipeline_cache: PipelineCache,
}

impl AerogpuD3d11Executor {
    pub async fn new_for_tests() -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir = std::env::temp_dir()
                    .join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::default();

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

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 aerogpu_cmd test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        let fallback_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu_cmd fallback texture"),
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
        let fallback_texture_view =
            fallback_texture.create_view(&wgpu::TextureViewDescriptor::default());
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &fallback_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0u8; 4],
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

        let fallback_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aerogpu_cmd fallback sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let fallback_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu_cmd fallback uniform buffer"),
            size: 65536,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        {
            let mut mapped = fallback_uniform_buffer.slice(..).get_mapped_range_mut();
            mapped.fill(0);
        }
        fallback_uniform_buffer.unmap();

        let caps = GpuCapabilities::from_device(&device);
        let pipeline_cache = PipelineCache::new(PipelineCacheConfig::default(), caps);

        Ok(Self {
            device,
            queue,
            resources: AerogpuD3d11Resources::default(),
            state: AerogpuD3d11State::default(),
            bind_group_layout_cache: BindGroupLayoutCache::new(),
            pipeline_layout_cache: HashMap::new(),
            fallback_texture_view,
            fallback_sampler,
            fallback_uniform_buffer,
            pipeline_cache,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn reset(&mut self) {
        self.resources = AerogpuD3d11Resources::default();
        self.state = AerogpuD3d11State::default();
        self.pipeline_cache.clear();
    }

    pub fn poll_wait(&self) {
        self.device.poll(wgpu::Maintain::Wait);
    }

    pub fn texture_size(&self, texture_id: u32) -> Result<(u32, u32)> {
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;
        Ok((texture.desc.width, texture.desc.height))
    }

    pub async fn read_texture_rgba8(&self, texture_id: u32) -> Result<Vec<u8>> {
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;

        let needs_bgra_swizzle = match texture.desc.format {
            wgpu::TextureFormat::Rgba8Unorm => false,
            wgpu::TextureFormat::Bgra8Unorm => true,
            other => {
                bail!("read_texture_rgba8 only supports Rgba8Unorm/Bgra8Unorm (got {other:?})")
            }
        };

        let width = texture.desc.width;
        let height = texture.desc.height;

        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = ((unpadded_bytes_per_row + align - 1) / align) * align;
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu_cmd read_texture staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd read_texture encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture.texture,
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
        self.device.poll(wgpu::Maintain::Wait);
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

    pub fn execute_cmd_stream(
        &mut self,
        stream_bytes: &[u8],
        allocs: Option<&[AerogpuAllocEntry]>,
        guest_mem: &dyn GuestMemory,
    ) -> Result<ExecuteReport> {
        let iter = AerogpuCmdStreamIter::new(stream_bytes)
            .map_err(|e| anyhow!("aerogpu_cmd: invalid cmd stream: {e:?}"))?;
        let stream_size = iter.header().size_bytes as usize;
        let mut iter = iter.peekable();

        let alloc_map = AllocTable::new(allocs.unwrap_or(&[]))?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu_cmd encoder"),
            });

        let mut report = ExecuteReport::default();

        let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
        while let Some(next) = iter.peek() {
            let (cmd_size, opcode) = match next {
                Ok(packet) => (packet.hdr.size_bytes as usize, packet.hdr.opcode),
                Err(err) => {
                    return Err(anyhow!(
                        "aerogpu_cmd: invalid cmd header @0x{cursor:x}: {err:?}"
                    ));
                }
            };
            let cmd_end = cursor
                .checked_add(cmd_size)
                .ok_or_else(|| anyhow!("aerogpu_cmd: cmd size overflow"))?;
            let cmd_bytes = stream_bytes
                .get(cursor..cmd_end)
                .ok_or_else(|| {
                    anyhow!(
                        "aerogpu_cmd: cmd overruns stream: cursor=0x{cursor:x} cmd_size=0x{cmd_size:x} stream_size=0x{stream_size:x}"
                    )
                })?;

            // Commands that need a render-pass boundary are handled by ending any
            // in-flight pass before processing the opcode.
            match opcode {
                OPCODE_DRAW | OPCODE_DRAW_INDEXED => {
                    self.exec_render_pass_load(
                        &mut encoder,
                        &mut iter,
                        &mut cursor,
                        stream_bytes,
                        stream_size,
                        &alloc_map,
                        guest_mem,
                        &mut report,
                    )?;
                    continue;
                }
                _ => {}
            }

            // Non-draw commands are processed directly.
            iter.next()
                .expect("peeked Some")
                .map_err(|err| anyhow!("aerogpu_cmd: invalid cmd header @0x{cursor:x}: {err:?}"))?;
            self.exec_non_draw_command(
                &mut encoder,
                opcode,
                cmd_bytes,
                &alloc_map,
                guest_mem,
                &mut report,
            )?;

            report.commands = report.commands.saturating_add(1);
            cursor = cmd_end;
        }

        self.queue.submit([encoder.finish()]);
        Ok(report)
    }

    fn exec_non_draw_command(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        opcode: u32,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &dyn GuestMemory,
        report: &mut ExecuteReport,
    ) -> Result<()> {
        match opcode {
            OPCODE_NOP => Ok(()),
            OPCODE_DEBUG_MARKER => Ok(()),
            OPCODE_CREATE_BUFFER => self.exec_create_buffer(cmd_bytes, allocs),
            OPCODE_CREATE_TEXTURE2D => self.exec_create_texture2d(cmd_bytes, allocs),
            OPCODE_DESTROY_RESOURCE => self.exec_destroy_resource(cmd_bytes),
            OPCODE_RESOURCE_DIRTY_RANGE => self.exec_resource_dirty_range(cmd_bytes),
            OPCODE_UPLOAD_RESOURCE => self.exec_upload_resource(cmd_bytes),
            OPCODE_COPY_BUFFER => self.exec_copy_buffer(encoder, cmd_bytes, allocs, guest_mem),
            OPCODE_COPY_TEXTURE2D => {
                self.exec_copy_texture2d(encoder, cmd_bytes, allocs, guest_mem)
            }
            OPCODE_CREATE_SHADER_DXBC => self.exec_create_shader_dxbc(cmd_bytes),
            OPCODE_DESTROY_SHADER => self.exec_destroy_shader(cmd_bytes),
            OPCODE_BIND_SHADERS => self.exec_bind_shaders(cmd_bytes),
            OPCODE_CREATE_INPUT_LAYOUT => self.exec_create_input_layout(cmd_bytes),
            OPCODE_DESTROY_INPUT_LAYOUT => self.exec_destroy_input_layout(cmd_bytes),
            OPCODE_SET_INPUT_LAYOUT => self.exec_set_input_layout(cmd_bytes),
            OPCODE_SET_RENDER_TARGETS => self.exec_set_render_targets(cmd_bytes),
            OPCODE_SET_VIEWPORT => self.exec_set_viewport(cmd_bytes),
            OPCODE_SET_SCISSOR => self.exec_set_scissor(cmd_bytes),
            OPCODE_SET_VERTEX_BUFFERS => self.exec_set_vertex_buffers(cmd_bytes),
            OPCODE_SET_INDEX_BUFFER => self.exec_set_index_buffer(cmd_bytes),
            OPCODE_SET_PRIMITIVE_TOPOLOGY => self.exec_set_primitive_topology(cmd_bytes),
            OPCODE_SET_TEXTURE => self.exec_set_texture(cmd_bytes),
            OPCODE_SET_SAMPLER_STATE => self.exec_set_sampler_state(cmd_bytes),
            OPCODE_CREATE_SAMPLER => self.exec_create_sampler(cmd_bytes),
            OPCODE_DESTROY_SAMPLER => self.exec_destroy_sampler(cmd_bytes),
            OPCODE_SET_SAMPLERS => self.exec_set_samplers(cmd_bytes),
            OPCODE_SET_CONSTANT_BUFFERS => self.exec_set_constant_buffers(cmd_bytes),
            OPCODE_CLEAR => self.exec_clear(encoder, cmd_bytes, allocs, guest_mem),
            OPCODE_PRESENT => self.exec_present(encoder, cmd_bytes, report),
            OPCODE_PRESENT_EX => self.exec_present_ex(encoder, cmd_bytes, report),
            OPCODE_FLUSH => self.exec_flush(encoder),
            // Known-but-ignored state that should not crash bring-up.
            OPCODE_SET_BLEND_STATE => self.exec_set_blend_state(cmd_bytes),
            OPCODE_SET_DEPTH_STENCIL_STATE => self.exec_set_depth_stencil_state(cmd_bytes),
            OPCODE_SET_RASTERIZER_STATE => self.exec_set_rasterizer_state(cmd_bytes),
            _ => {
                report.unknown_opcodes = report.unknown_opcodes.saturating_add(1);
                Ok(())
            }
        }
    }

    /// Execute a batch of draw/state commands inside a single render pass.
    ///
    /// This function is entered when the main stream parser sees a `DRAW`/`DRAW_INDEXED`
    /// opcode while no render pass is active. It begins a pass with `LoadOp::Load`,
    /// then continues consuming subsequent commands until a pass-ending opcode is
    /// reached (SET_RENDER_TARGETS, CLEAR, PRESENT, FLUSH, ...).
    fn exec_render_pass_load<'a>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        iter: &mut core::iter::Peekable<AerogpuCmdStreamIter<'a>>,
        cursor: &mut usize,
        stream_bytes: &'a [u8],
        stream_size: usize,
        allocs: &AllocTable,
        guest_mem: &dyn GuestMemory,
        report: &mut ExecuteReport,
    ) -> Result<()> {
        if self.state.render_targets.is_empty() {
            bail!("aerogpu_cmd: draw without bound render target");
        }

        let render_targets = self.state.render_targets.clone();
        let depth_stencil = self.state.depth_stencil;
        for &handle in &render_targets {
            self.ensure_texture_uploaded(handle, allocs, guest_mem)?;
        }
        if let Some(handle) = depth_stencil {
            self.ensure_texture_uploaded(handle, allocs, guest_mem)?;
        }

        // Upload any dirty resources used by the current input assembler bindings.
        let mut ia_buffers: Vec<u32> = self
            .state
            .vertex_buffers
            .iter()
            .flatten()
            .map(|b| b.buffer)
            .collect();
        if let Some(ib) = self.state.index_buffer {
            ia_buffers.push(ib.buffer);
        }
        for handle in ia_buffers {
            self.ensure_buffer_uploaded(handle, allocs, guest_mem)?;
        }

        // The upcoming render pass will write to bound targets. Invalidate any CPU shadow copies so
        // that later partial `UPLOAD_RESOURCE` operations don't accidentally overwrite GPU-produced
        // contents.
        for &handle in &render_targets {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
            }
        }
        if let Some(handle) = depth_stencil {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
            }
        }

        let (vs_reflection, ps_reflection) = {
            let vs_handle = self
                .state
                .vs
                .ok_or_else(|| anyhow!("render draw without bound VS"))?;
            let ps_handle = self
                .state
                .ps
                .ok_or_else(|| anyhow!("render draw without bound PS"))?;

            let vs = self
                .resources
                .shaders
                .get(&vs_handle)
                .ok_or_else(|| anyhow!("unknown VS shader {vs_handle}"))?;
            let ps = self
                .resources
                .shaders
                .get(&ps_handle)
                .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?;

            if vs.stage != ShaderStage::Vertex {
                bail!("shader {vs_handle} is not a vertex shader");
            }
            if ps.stage != ShaderStage::Pixel {
                bail!("shader {ps_handle} is not a pixel shader");
            }

            (vs.reflection.clone(), ps.reflection.clone())
        };

        let prepared_bindings = self.prepare_pipeline_bindings(&vs_reflection, &ps_reflection)?;

        // Upload any dirty resources referenced by shader bindings (cbuffers + SRVs).
        for binding in &prepared_bindings.bindings {
            match binding.kind {
                BindingKind::Texture2D { .. } => {
                    if let Some(handle) = resolve_texture_binding(&self.state, binding) {
                        self.ensure_texture_uploaded(handle, allocs, guest_mem)?;
                    }
                }
                BindingKind::ConstantBuffer { .. } => {
                    if let Some(cb) = resolve_constant_buffer_binding(&self.state, binding) {
                        self.ensure_buffer_uploaded(cb.buffer, allocs, guest_mem)?;
                    }
                }
                BindingKind::Sampler { .. } => {}
            }
        }

        let (_pipeline_key, pipeline, wgpu_slot_to_d3d_slot) =
            get_or_create_render_pipeline_for_state(
                &self.device,
                &mut self.pipeline_cache,
                prepared_bindings.pipeline_layout.as_ref(),
                &mut self.resources,
                &self.state,
                prepared_bindings.layout_key.clone(),
            )?;

        let bind_groups = build_bind_groups(
            &self.device,
            encoder,
            &prepared_bindings.bindings,
            &prepared_bindings.group_layouts,
            &self.state,
            &self.resources,
            &self.fallback_texture_view,
            &self.fallback_sampler,
            &self.fallback_uniform_buffer,
        )?;

        let state = &self.state;
        let resources = &self.resources;

        let (color_attachments, depth_stencil_attachment) =
            build_render_pass_attachments(resources, state, wgpu::LoadOp::Load)?;

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu_cmd render pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // Apply dynamic state once at pass start.
        let rt_dims = state
            .render_targets
            .first()
            .and_then(|rt| resources.textures.get(rt))
            .map(|tex| (tex.desc.width, tex.desc.height));

        if let Some(vp) = state.viewport {
            if vp.x.is_finite()
                && vp.y.is_finite()
                && vp.width.is_finite()
                && vp.height.is_finite()
                && vp.min_depth.is_finite()
                && vp.max_depth.is_finite()
            {
                if let Some((rt_w, rt_h)) = rt_dims {
                    let max_w = rt_w as f32;
                    let max_h = rt_h as f32;

                    let left = vp.x.max(0.0);
                    let top = vp.y.max(0.0);
                    let right = (vp.x + vp.width).max(0.0).min(max_w);
                    let bottom = (vp.y + vp.height).max(0.0).min(max_h);
                    let width = (right - left).max(0.0);
                    let height = (bottom - top).max(0.0);

                    if width > 0.0 && height > 0.0 {
                        let mut min_depth = vp.min_depth.clamp(0.0, 1.0);
                        let mut max_depth = vp.max_depth.clamp(0.0, 1.0);
                        if min_depth > max_depth {
                            std::mem::swap(&mut min_depth, &mut max_depth);
                        }
                        pass.set_viewport(left, top, width, height, min_depth, max_depth);
                    }
                } else {
                    pass.set_viewport(vp.x, vp.y, vp.width, vp.height, vp.min_depth, vp.max_depth);
                }
            }
        }
        if state.scissor_enable {
            if let Some(sc) = state.scissor {
                if let Some((rt_w, rt_h)) = rt_dims {
                    let x = sc.x.min(rt_w);
                    let y = sc.y.min(rt_h);
                    let width = sc.width.min(rt_w.saturating_sub(x));
                    let height = sc.height.min(rt_h.saturating_sub(y));
                    if width > 0 && height > 0 {
                        pass.set_scissor_rect(x, y, width, height);
                    }
                }
            }
        }
        pass.set_blend_constant(wgpu::Color {
            r: state.blend_constant[0] as f64,
            g: state.blend_constant[1] as f64,
            b: state.blend_constant[2] as f64,
            a: state.blend_constant[3] as f64,
        });

        pass.set_pipeline(pipeline);
        for (group, bind_group) in bind_groups.iter().enumerate() {
            pass.set_bind_group(group as u32, bind_group, &[]);
        }

        for (wgpu_slot, d3d_slot) in wgpu_slot_to_d3d_slot.iter().copied().enumerate() {
            let slot = d3d_slot as usize;
            let Some(vb) = state.vertex_buffers.get(slot).and_then(|v| *v) else {
                bail!("input layout requires vertex buffer slot {d3d_slot}");
            };
            let buf = resources
                .buffers
                .get(&vb.buffer)
                .ok_or_else(|| anyhow!("unknown vertex buffer {}", vb.buffer))?;
            pass.set_vertex_buffer(wgpu_slot as u32, buf.buffer.slice(vb.offset_bytes..));
        }
        if let Some(ib) = state.index_buffer {
            let buf = resources
                .buffers
                .get(&ib.buffer)
                .ok_or_else(|| anyhow!("unknown index buffer {}", ib.buffer))?;
            pass.set_index_buffer(buf.buffer.slice(ib.offset_bytes..), ib.format);
        }

        loop {
            let Some(next) = iter.peek() else {
                break;
            };
            let (cmd_size, opcode) = match next {
                Ok(packet) => (packet.hdr.size_bytes as usize, packet.hdr.opcode),
                Err(err) => {
                    return Err(anyhow!(
                        "aerogpu_cmd: invalid cmd header @0x{:x}: {err:?}",
                        *cursor
                    ));
                }
            };

            match opcode {
                OPCODE_DRAW | OPCODE_DRAW_INDEXED | OPCODE_NOP | OPCODE_DEBUG_MARKER => {}
                _ => break, // leave the opcode for the outer loop
            }

            let cmd_end = cursor
                .checked_add(cmd_size)
                .ok_or_else(|| anyhow!("aerogpu_cmd: cmd size overflow"))?;
            let cmd_bytes = stream_bytes
                .get(*cursor..cmd_end)
                .ok_or_else(|| {
                    anyhow!(
                        "aerogpu_cmd: cmd overruns stream: cursor=0x{:x} cmd_size=0x{:x} stream_size=0x{:x}",
                        *cursor,
                        cmd_size,
                        stream_size
                    )
                })?;
            iter.next().expect("peeked Some").map_err(|err| {
                anyhow!("aerogpu_cmd: invalid cmd header @0x{:x}: {err:?}", *cursor)
            })?;

            match opcode {
                OPCODE_DRAW => {
                    if (state.sample_mask & 1) != 0 {
                        exec_draw(&mut pass, cmd_bytes)?;
                    }
                }
                OPCODE_DRAW_INDEXED => {
                    if state.index_buffer.is_none() {
                        bail!("DRAW_INDEXED without index buffer");
                    }
                    if (state.sample_mask & 1) != 0 {
                        exec_draw_indexed(&mut pass, cmd_bytes)?;
                    }
                }
                OPCODE_NOP | OPCODE_DEBUG_MARKER => {}
                _ => {}
            }

            report.commands = report.commands.saturating_add(1);
            *cursor = cmd_end;
        }

        drop(pass);
        Ok(())
    }
    fn exec_create_buffer(&mut self, cmd_bytes: &[u8], allocs: &AllocTable) -> Result<()> {
        // struct aerogpu_cmd_create_buffer (40 bytes)
        if cmd_bytes.len() != 40 {
            bail!("CREATE_BUFFER: expected 40 bytes, got {}", cmd_bytes.len());
        }
        let buffer_handle = read_u32_le(cmd_bytes, 8)?;
        let usage_flags = read_u32_le(cmd_bytes, 12)?;
        let size_bytes = read_u64_le(cmd_bytes, 16)?;
        let backing_alloc_id = read_u32_le(cmd_bytes, 24)?;
        let backing_offset_bytes = read_u32_le(cmd_bytes, 28)?;

        if size_bytes == 0 {
            bail!("CREATE_BUFFER: size_bytes must be > 0");
        }

        let usage = map_buffer_usage_flags(usage_flags);
        let gpu_size = align_copy_buffer_size(size_bytes)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu buffer"),
            size: gpu_size,
            usage,
            mapped_at_creation: false,
        });

        let backing = if backing_alloc_id != 0 {
            allocs.validate_range(backing_alloc_id, backing_offset_bytes as u64, size_bytes)?;
            Some(ResourceBacking {
                alloc_id: backing_alloc_id,
                offset_bytes: backing_offset_bytes as u64,
            })
        } else {
            None
        };

        let mut res = BufferResource {
            buffer,
            size: size_bytes,
            gpu_size,
            backing,
            dirty: None,
        };
        if res.backing.is_some() {
            res.mark_dirty(0..size_bytes);
        }

        self.resources.buffers.insert(buffer_handle, res);
        Ok(())
    }

    fn exec_create_texture2d(&mut self, cmd_bytes: &[u8], allocs: &AllocTable) -> Result<()> {
        // struct aerogpu_cmd_create_texture2d (56 bytes)
        if cmd_bytes.len() != 56 {
            bail!(
                "CREATE_TEXTURE2D: expected 56 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let texture_handle = read_u32_le(cmd_bytes, 8)?;
        let usage_flags = read_u32_le(cmd_bytes, 12)?;
        let format_u32 = read_u32_le(cmd_bytes, 16)?;
        let width = read_u32_le(cmd_bytes, 20)?;
        let height = read_u32_le(cmd_bytes, 24)?;
        let mip_levels = read_u32_le(cmd_bytes, 28)?;
        let array_layers = read_u32_le(cmd_bytes, 32)?;
        let row_pitch_bytes = read_u32_le(cmd_bytes, 36)?;
        let backing_alloc_id = read_u32_le(cmd_bytes, 40)?;
        let backing_offset_bytes = read_u32_le(cmd_bytes, 44)?;

        if width == 0 || height == 0 {
            bail!("CREATE_TEXTURE2D: width/height must be non-zero");
        }
        if mip_levels == 0 || array_layers == 0 {
            bail!("CREATE_TEXTURE2D: mip_levels/array_layers must be >= 1");
        }

        let format = map_aerogpu_texture_format(format_u32)?;
        let usage = map_texture_usage_flags(usage_flags);
        let required_row_pitch = width
            .checked_mul(bytes_per_texel(format)?)
            .ok_or_else(|| anyhow!("CREATE_TEXTURE2D: row_pitch overflow"))?;
        if row_pitch_bytes != 0 && row_pitch_bytes < required_row_pitch {
            bail!(
                "CREATE_TEXTURE2D: row_pitch_bytes {row_pitch_bytes} is smaller than required {required_row_pitch}"
            );
        }
        if backing_alloc_id != 0 && row_pitch_bytes == 0 {
            bail!("CREATE_TEXTURE2D: row_pitch_bytes is required for allocation-backed textures");
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: array_layers,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let backing = if backing_alloc_id != 0 {
            // Validate that the allocation can hold all mips/layers. We only carry a single
            // `row_pitch_bytes` value in the protocol, so we use the same conservative estimate as
            // the generic AeroGPU command processor (shift row pitch/height per mip).
            let mut total_size = 0u64;
            for level in 0..mip_levels {
                let level_row_pitch = (u64::from(row_pitch_bytes) >> level).max(1);
                let level_height = (u64::from(height) >> level).max(1);
                let level_size = level_row_pitch
                    .checked_mul(level_height)
                    .ok_or_else(|| anyhow!("CREATE_TEXTURE2D: size overflow"))?;
                total_size = total_size
                    .checked_add(level_size)
                    .ok_or_else(|| anyhow!("CREATE_TEXTURE2D: size overflow"))?;
            }
            total_size = total_size
                .checked_mul(u64::from(array_layers))
                .ok_or_else(|| anyhow!("CREATE_TEXTURE2D: size overflow"))?;

            allocs.validate_range(backing_alloc_id, backing_offset_bytes as u64, total_size)?;
            Some(ResourceBacking {
                alloc_id: backing_alloc_id,
                offset_bytes: backing_offset_bytes as u64,
            })
        } else {
            None
        };

        self.resources.textures.insert(
            texture_handle,
            Texture2dResource {
                texture,
                view,
                desc: Texture2dDesc {
                    width,
                    height,
                    mip_level_count: mip_levels,
                    array_layers,
                    format,
                },
                backing,
                row_pitch_bytes,
                dirty: backing.is_some(),
                host_shadow: None,
            },
        );
        Ok(())
    }

    fn exec_destroy_resource(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_destroy_resource (16 bytes)
        if cmd_bytes.len() != 16 {
            bail!(
                "DESTROY_RESOURCE: expected 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;

        self.resources.buffers.remove(&handle);
        self.resources.textures.remove(&handle);

        // Clean up bindings in state.
        self.state.render_targets.retain(|&rt| rt != handle);
        if self.state.depth_stencil == Some(handle) {
            self.state.depth_stencil = None;
        }
        for slot in &mut self.state.vertex_buffers {
            if slot.is_some_and(|b| b.buffer == handle) {
                *slot = None;
            }
        }
        if self.state.index_buffer.is_some_and(|b| b.buffer == handle) {
            self.state.index_buffer = None;
        }
        for slots in [
            &mut self.state.textures_vs,
            &mut self.state.textures_ps,
            &mut self.state.textures_cs,
        ] {
            for slot in slots {
                if *slot == Some(handle) {
                    *slot = None;
                }
            }
        }
        for slots in [
            &mut self.state.constant_buffers_vs,
            &mut self.state.constant_buffers_ps,
            &mut self.state.constant_buffers_cs,
        ] {
            for slot in slots {
                if slot.is_some_and(|b| b.buffer == handle) {
                    *slot = None;
                }
            }
        }

        Ok(())
    }

    fn exec_resource_dirty_range(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_resource_dirty_range (32 bytes)
        if cmd_bytes.len() != 32 {
            bail!(
                "RESOURCE_DIRTY_RANGE: expected 32 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        let offset = read_u64_le(cmd_bytes, 16)?;
        let size = read_u64_le(cmd_bytes, 24)?;

        if let Some(buf) = self.resources.buffers.get_mut(&handle) {
            let end = offset.saturating_add(size).min(buf.size);
            let start = offset.min(end);
            buf.mark_dirty(start..end);
        } else if let Some(tex) = self.resources.textures.get_mut(&handle) {
            tex.dirty = true;
        }
        Ok(())
    }

    fn exec_upload_resource(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, data) = decode_cmd_upload_resource_payload_le(cmd_bytes)
            .map_err(|e| anyhow!("UPLOAD_RESOURCE: invalid payload: {e:?}"))?;
        let handle = cmd.resource_handle;
        let offset = cmd.offset_bytes;
        let size = cmd.size_bytes;

        if let Some(buf) = self.resources.buffers.get(&handle) {
            let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
            if offset % alignment != 0 {
                bail!("UPLOAD_RESOURCE: buffer offset {offset} does not respect COPY_BUFFER_ALIGNMENT");
            }
            if offset.saturating_add(size) > buf.size {
                bail!("UPLOAD_RESOURCE: buffer upload out of bounds");
            }

            // `wgpu::Queue::write_buffer` requires the write size be a multiple of
            // `COPY_BUFFER_ALIGNMENT` (4). The AeroGPU command stream is byte-granular (e.g. index
            // buffers can be 3x u16 = 6 bytes), so we pad writes that reach the end of the buffer.
            if size % alignment != 0 {
                if offset.saturating_add(size) != buf.size {
                    bail!(
                        "UPLOAD_RESOURCE: unaligned buffer upload is only supported when writing to the end of the buffer"
                    );
                }
                let size_usize: usize = size
                    .try_into()
                    .map_err(|_| anyhow!("UPLOAD_RESOURCE: size_bytes out of range"))?;
                let padded = align4(size_usize);
                let mut tmp = vec![0u8; padded];
                tmp[..size_usize].copy_from_slice(data);

                let end = offset
                    .checked_add(padded as u64)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: upload range overflows u64"))?;
                if end > buf.gpu_size {
                    bail!("UPLOAD_RESOURCE: padded upload overruns wgpu buffer allocation");
                }

                self.queue.write_buffer(&buf.buffer, offset, &tmp);
            } else {
                self.queue.write_buffer(&buf.buffer, offset, data);
            }
            if let Some(buf_mut) = self.resources.buffers.get_mut(&handle) {
                // Uploaded data is now current on the GPU; clear dirty ranges.
                if let Some(dirty) = buf_mut.dirty.take() {
                    // If the dirty range extends outside the uploaded region, keep it.
                    let uploaded = offset..offset.saturating_add(size);
                    if dirty.start < uploaded.start || dirty.end > uploaded.end {
                        buf_mut.dirty = Some(dirty);
                    }
                }
            }
            return Ok(());
        }

        if let Some(tex) = self.resources.textures.get_mut(&handle) {
            // Texture uploads are expressed as a linear byte range into mip0/layer0.
            //
            // WebGPU uploads are 2D; for partial updates we patch into a CPU shadow buffer and then
            // re-upload the full texture.
            let bytes_per_row = if tex.row_pitch_bytes != 0 {
                tex.row_pitch_bytes
            } else {
                tex.desc
                    .width
                    .checked_mul(bytes_per_texel(tex.desc.format)?)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: bytes_per_row overflow"))?
            };
            let expected = (bytes_per_row as u64).saturating_mul(tex.desc.height as u64);

            let end = offset
                .checked_add(size)
                .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: upload range overflows u64"))?;
            if end > expected {
                bail!("UPLOAD_RESOURCE: texture upload out of bounds");
            }

            let expected_usize: usize = expected
                .try_into()
                .map_err(|_| anyhow!("UPLOAD_RESOURCE: texture upload size out of range"))?;
            let offset_usize: usize = offset
                .try_into()
                .map_err(|_| anyhow!("UPLOAD_RESOURCE: offset out of range"))?;
            let end_usize: usize = end
                .try_into()
                .map_err(|_| anyhow!("UPLOAD_RESOURCE: end out of range"))?;

            if offset == 0 && size == expected {
                write_texture_linear(&self.queue, &tex.texture, tex.desc, bytes_per_row, data)?;
                tex.host_shadow = Some(data.to_vec());
                tex.dirty = false;
                return Ok(());
            }

            let shadow = tex.host_shadow.as_mut().ok_or_else(|| {
                anyhow!("UPLOAD_RESOURCE: partial texture uploads require a prior full upload")
            })?;
            if shadow.len() != expected_usize {
                bail!("UPLOAD_RESOURCE: internal shadow size mismatch");
            }
            shadow[offset_usize..end_usize].copy_from_slice(data);

            write_texture_linear(&self.queue, &tex.texture, tex.desc, bytes_per_row, shadow)?;
            tex.dirty = false;
            return Ok(());
        }

        Ok(())
    }

    fn exec_copy_buffer(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &dyn GuestMemory,
    ) -> Result<()> {
        let cmd = decode_cmd_copy_buffer_le(cmd_bytes)
            .map_err(|e| anyhow!("COPY_BUFFER: invalid payload: {e:?}"))?;
        // `AerogpuCmdCopyBuffer` is `repr(C, packed)` (ABI mirror); copy out fields before use to
        // avoid taking references to packed fields.
        let dst_buffer = cmd.dst_buffer;
        let src_buffer = cmd.src_buffer;
        let dst_offset_bytes = cmd.dst_offset_bytes;
        let src_offset_bytes = cmd.src_offset_bytes;
        let size_bytes = cmd.size_bytes;
        let flags = cmd.flags;

        let writeback = (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            bail!("COPY_BUFFER: unknown flags {flags:#x}");
        }
        if size_bytes == 0 {
            return Ok(());
        }
        if dst_buffer == 0 || src_buffer == 0 {
            bail!("COPY_BUFFER: resource handles must be non-zero");
        }
        if dst_buffer == src_buffer {
            bail!("COPY_BUFFER: src==dst is not supported");
        }

        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        if dst_offset_bytes % alignment != 0 || src_offset_bytes % alignment != 0 {
            bail!(
                "COPY_BUFFER: offsets must be multiples of {alignment} (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes})"
            );
        }

        let dst_copy_end = dst_offset_bytes
            .checked_add(size_bytes)
            .ok_or_else(|| anyhow!("COPY_BUFFER: dst range overflows u64"))?;

        // Ensure the source buffer reflects any CPU writes from guest memory before copying.
        self.ensure_buffer_uploaded(src_buffer, allocs, guest_mem)?;

        let dst_backing = if writeback {
            let dst = self
                .resources
                .buffers
                .get(&dst_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))?;
            dst.backing.ok_or_else(|| {
                anyhow!(
                    "COPY_BUFFER: WRITEBACK_DST requires dst buffer to be guest-backed (handle={dst_buffer})"
                )
            })?
        } else {
            ResourceBacking {
                alloc_id: 0,
                offset_bytes: 0,
            }
        };

        // If the destination is guest-backed and has pending uploads outside the copied region,
        // upload them now so untouched bytes remain correct.
        let needs_dst_upload = {
            let dst = self
                .resources
                .buffers
                .get(&dst_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))?;
            match dst.dirty.as_ref() {
                Some(dirty) if dst.backing.is_some() => {
                    dirty.start < dst_offset_bytes || dirty.end > dst_copy_end
                }
                _ => false,
            }
        };
        if needs_dst_upload {
            self.ensure_buffer_uploaded(dst_buffer, allocs, guest_mem)?;
        }

        let mut staging: Option<wgpu::Buffer> = None;
        let mut copy_size_aligned = size_bytes;

        // Encode the copy.
        {
            let src = self
                .resources
                .buffers
                .get(&src_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown src buffer {src_buffer}"))?;
            let dst = self
                .resources
                .buffers
                .get(&dst_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))?;

            let src_end = src_offset_bytes
                .checked_add(size_bytes)
                .ok_or_else(|| anyhow!("COPY_BUFFER: src range overflows u64"))?;
            if src_end > src.size {
                bail!(
                    "COPY_BUFFER: src out of bounds: offset=0x{:x} size=0x{:x} buffer_size=0x{:x}",
                    src_offset_bytes,
                    size_bytes,
                    src.size
                );
            }
            if dst_copy_end > dst.size {
                bail!(
                    "COPY_BUFFER: dst out of bounds: offset=0x{:x} size=0x{:x} buffer_size=0x{:x}",
                    dst_offset_bytes,
                    size_bytes,
                    dst.size
                );
            }

            if size_bytes % alignment != 0 {
                if src_end != src.size || dst_copy_end != dst.size {
                    bail!(
                        "COPY_BUFFER: size_bytes must be a multiple of {alignment} unless copying to the end of both buffers (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes} size_bytes={size_bytes} dst_size={} src_size={})",
                        dst.size,
                        src.size
                    );
                }
                copy_size_aligned = align_copy_buffer_size(size_bytes)?;
            }
            let src_copy_end_aligned = src_offset_bytes
                .checked_add(copy_size_aligned)
                .ok_or_else(|| anyhow!("COPY_BUFFER: aligned src range overflows u64"))?;
            let dst_copy_end_aligned = dst_offset_bytes
                .checked_add(copy_size_aligned)
                .ok_or_else(|| anyhow!("COPY_BUFFER: aligned dst range overflows u64"))?;
            if src_copy_end_aligned > src.gpu_size || dst_copy_end_aligned > dst.gpu_size {
                bail!("COPY_BUFFER: aligned copy range overruns wgpu buffer allocation");
            }

            encoder.copy_buffer_to_buffer(
                &src.buffer,
                src_offset_bytes,
                &dst.buffer,
                dst_offset_bytes,
                copy_size_aligned,
            );

            if writeback {
                let staging_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aerogpu_cmd copy_buffer writeback staging"),
                    size: copy_size_aligned,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                encoder.copy_buffer_to_buffer(
                    &dst.buffer,
                    dst_offset_bytes,
                    &staging_buf,
                    0,
                    copy_size_aligned,
                );
                staging = Some(staging_buf);
            }
        }

        if writeback {
            #[cfg(target_arch = "wasm32")]
            {
                bail!("COPY_BUFFER: AEROGPU_COPY_FLAG_WRITEBACK_DST is not supported on wasm yet");
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                let Some(staging) = staging else {
                    bail!("COPY_BUFFER: internal error: missing staging buffer for writeback");
                };

                let dst_offset = dst_backing
                    .offset_bytes
                    .checked_add(dst_offset_bytes)
                    .ok_or_else(|| anyhow!("COPY_BUFFER: dst backing offset overflow"))?;
                let dst_gpa =
                    allocs.validate_write_range(dst_backing.alloc_id, dst_offset, size_bytes)?;

                let new_encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("aerogpu_cmd encoder after COPY_BUFFER writeback"),
                        });
                let finished = std::mem::replace(encoder, new_encoder).finish();
                self.queue.submit([finished]);

                let slice = staging.slice(..);
                let state = std::sync::Arc::new((
                    std::sync::Mutex::new(None::<Result<(), wgpu::BufferAsyncError>>),
                    std::sync::Condvar::new(),
                ));
                let state_clone = state.clone();
                slice.map_async(wgpu::MapMode::Read, move |res| {
                    let (lock, cv) = &*state_clone;
                    *lock.lock().unwrap() = Some(res);
                    cv.notify_one();
                });
                self.device.poll(wgpu::Maintain::Wait);

                let (lock, cv) = &*state;
                let mut guard = lock.lock().unwrap();
                while guard.is_none() {
                    guard = cv.wait(guard).unwrap();
                }
                guard
                    .take()
                    .unwrap()
                    .map_err(|e| anyhow!("COPY_BUFFER: writeback map_async failed: {e:?}"))?;

                let mapped = slice.get_mapped_range();
                let len: usize = size_bytes
                    .try_into()
                    .map_err(|_| anyhow!("COPY_BUFFER: size_bytes out of range"))?;
                guest_mem
                    .write(dst_gpa, &mapped[..len])
                    .map_err(anyhow_guest_mem)?;
                drop(mapped);
                staging.unmap();
            }
        }

        // The destination GPU buffer content has changed; discard any pending "dirty" ranges that
        // would otherwise cause us to overwrite the copy with stale guest-memory contents.
        if let Some(dst) = self.resources.buffers.get_mut(&dst_buffer) {
            dst.dirty = None;
        }

        Ok(())
    }

    fn exec_copy_texture2d(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &dyn GuestMemory,
    ) -> Result<()> {
        let cmd = decode_cmd_copy_texture2d_le(cmd_bytes)
            .map_err(|e| anyhow!("COPY_TEXTURE2D: invalid payload: {e:?}"))?;
        // `AerogpuCmdCopyTexture2d` is `repr(C, packed)` (ABI mirror); copy out fields before use
        // to avoid taking references to packed fields.
        let dst_texture = cmd.dst_texture;
        let src_texture = cmd.src_texture;
        let dst_mip_level = cmd.dst_mip_level;
        let dst_array_layer = cmd.dst_array_layer;
        let src_mip_level = cmd.src_mip_level;
        let src_array_layer = cmd.src_array_layer;
        let dst_x = cmd.dst_x;
        let dst_y = cmd.dst_y;
        let src_x = cmd.src_x;
        let src_y = cmd.src_y;
        let width = cmd.width;
        let height = cmd.height;
        let flags = cmd.flags;

        let writeback = (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            bail!("COPY_TEXTURE2D: unknown flags {flags:#x}");
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        if dst_texture == 0 || src_texture == 0 {
            bail!("COPY_TEXTURE2D: resource handles must be non-zero");
        }

        if writeback && (dst_mip_level != 0 || dst_array_layer != 0) {
            bail!(
                "COPY_TEXTURE2D: WRITEBACK_DST is only supported for dst_mip_level=0 and dst_array_layer=0 (got mip={} layer={})",
                dst_mip_level,
                dst_array_layer
            );
        }

        // Ensure the source texture reflects any CPU writes from guest memory before copying.
        self.ensure_texture_uploaded(src_texture, allocs, guest_mem)?;

        // If the destination is guest-backed and dirty and we're only overwriting a sub-rectangle,
        // upload it now so the untouched pixels remain correct.
        let needs_dst_upload = {
            let dst = self
                .resources
                .textures
                .get(&dst_texture)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: unknown dst texture {dst_texture}"))?;
            if dst.backing.is_none() || !dst.dirty {
                false
            } else {
                let dst_is_base = dst_mip_level == 0 && dst_array_layer == 0;
                let covers_full = dst_x == 0
                    && dst_y == 0
                    && width == dst.desc.width
                    && height == dst.desc.height;
                dst_is_base && !covers_full
            }
        };
        if needs_dst_upload {
            self.ensure_texture_uploaded(dst_texture, allocs, guest_mem)?;
        }

        let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);

        let (dst_backing, dst_row_pitch_bytes) = if writeback {
            let dst = self
                .resources
                .textures
                .get(&dst_texture)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: unknown dst texture {dst_texture}"))?;
            (
                dst.backing.ok_or_else(|| {
                    anyhow!(
                        "COPY_TEXTURE2D: WRITEBACK_DST requires dst texture to be guest-backed (handle={dst_texture})"
                    )
                })?,
                dst.row_pitch_bytes,
            )
        } else {
            (
                ResourceBacking {
                    alloc_id: 0,
                    offset_bytes: 0,
                },
                0u32,
            )
        };

        let mut staging: Option<(wgpu::Buffer, u32, u32, u32)> = None;

        {
            let src = self
                .resources
                .textures
                .get(&src_texture)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: unknown src texture {src_texture}"))?;
            let dst = self
                .resources
                .textures
                .get(&dst_texture)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: unknown dst texture {dst_texture}"))?;

            if src_mip_level >= src.desc.mip_level_count {
                bail!(
                    "COPY_TEXTURE2D: src_mip_level {src_mip_level} out of range (mip_levels={})",
                    src.desc.mip_level_count
                );
            }
            if dst_mip_level >= dst.desc.mip_level_count {
                bail!(
                    "COPY_TEXTURE2D: dst_mip_level {dst_mip_level} out of range (mip_levels={})",
                    dst.desc.mip_level_count
                );
            }
            if src_array_layer >= src.desc.array_layers {
                bail!(
                    "COPY_TEXTURE2D: src_array_layer {src_array_layer} out of range (array_layers={})",
                    src.desc.array_layers
                );
            }
            if dst_array_layer >= dst.desc.array_layers {
                bail!(
                    "COPY_TEXTURE2D: dst_array_layer {dst_array_layer} out of range (array_layers={})",
                    dst.desc.array_layers
                );
            }

            if src.desc.format != dst.desc.format {
                bail!(
                    "COPY_TEXTURE2D: format mismatch: src={:?} dst={:?}",
                    src.desc.format,
                    dst.desc.format
                );
            }

            let src_w = mip_extent(src.desc.width, src_mip_level);
            let src_h = mip_extent(src.desc.height, src_mip_level);
            let dst_w = mip_extent(dst.desc.width, dst_mip_level);
            let dst_h = mip_extent(dst.desc.height, dst_mip_level);

            let src_x_end = src_x
                .checked_add(width)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: src_x+width overflows u32"))?;
            let src_y_end = src_y
                .checked_add(height)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: src_y+height overflows u32"))?;
            let dst_x_end = dst_x
                .checked_add(width)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_x+width overflows u32"))?;
            let dst_y_end = dst_y
                .checked_add(height)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_y+height overflows u32"))?;

            if src_x_end > src_w || src_y_end > src_h {
                bail!("COPY_TEXTURE2D: src rect out of bounds");
            }
            if dst_x_end > dst_w || dst_y_end > dst_h {
                bail!("COPY_TEXTURE2D: dst rect out of bounds");
            }

            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: &src.texture,
                    mip_level: src_mip_level,
                    origin: wgpu::Origin3d {
                        x: src_x,
                        y: src_y,
                        z: src_array_layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: &dst.texture,
                    mip_level: dst_mip_level,
                    origin: wgpu::Origin3d {
                        x: dst_x,
                        y: dst_y,
                        z: dst_array_layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );

            if writeback {
                let bytes_per_pixel = bytes_per_texel(dst.desc.format)?;
                let mip_w = mip_extent(dst.desc.width, dst_mip_level);
                let mip_h = mip_extent(dst.desc.height, dst_mip_level);
                let unpadded_bpr = mip_w
                    .checked_mul(bytes_per_pixel)
                    .ok_or_else(|| anyhow!("COPY_TEXTURE2D: bytes_per_row overflow"))?;
                let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                let padded_bpr = ((unpadded_bpr + align - 1) / align) * align;
                let buffer_size = (padded_bpr as u64)
                    .checked_mul(mip_h as u64)
                    .ok_or_else(|| anyhow!("COPY_TEXTURE2D: staging buffer size overflow"))?;
                let staging_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aerogpu_cmd copy_texture2d writeback staging"),
                    size: buffer_size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture: &dst.texture,
                        mip_level: dst_mip_level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: dst_array_layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &staging_buf,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(padded_bpr),
                            rows_per_image: Some(mip_h),
                        },
                    },
                    wgpu::Extent3d {
                        width: mip_w,
                        height: mip_h,
                        depth_or_array_layers: 1,
                    },
                );
                staging = Some((staging_buf, padded_bpr, unpadded_bpr, mip_h));
            }
        }

        if writeback {
            #[cfg(target_arch = "wasm32")]
            {
                bail!(
                    "COPY_TEXTURE2D: AEROGPU_COPY_FLAG_WRITEBACK_DST is not supported on wasm yet"
                );
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                let Some((staging, padded_bpr, unpadded_bpr, mip_h)) = staging else {
                    bail!("COPY_TEXTURE2D: internal error: missing staging buffer for writeback");
                };
                if dst_row_pitch_bytes == 0 {
                    bail!("COPY_TEXTURE2D: WRITEBACK_DST requires non-zero dst row_pitch_bytes");
                }
                let required = (dst_row_pitch_bytes as u64)
                    .checked_mul(mip_h as u64)
                    .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing size overflow"))?;
                let base_gpa = allocs.validate_write_range(
                    dst_backing.alloc_id,
                    dst_backing.offset_bytes,
                    required,
                )?;

                let new_encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("aerogpu_cmd encoder after COPY_TEXTURE2D writeback"),
                        });
                let finished = std::mem::replace(encoder, new_encoder).finish();
                self.queue.submit([finished]);

                let slice = staging.slice(..);
                let state = std::sync::Arc::new((
                    std::sync::Mutex::new(None::<Result<(), wgpu::BufferAsyncError>>),
                    std::sync::Condvar::new(),
                ));
                let state_clone = state.clone();
                slice.map_async(wgpu::MapMode::Read, move |res| {
                    let (lock, cv) = &*state_clone;
                    *lock.lock().unwrap() = Some(res);
                    cv.notify_one();
                });
                self.device.poll(wgpu::Maintain::Wait);

                let (lock, cv) = &*state;
                let mut guard = lock.lock().unwrap();
                while guard.is_none() {
                    guard = cv.wait(guard).unwrap();
                }
                guard
                    .take()
                    .unwrap()
                    .map_err(|e| anyhow!("COPY_TEXTURE2D: writeback map_async failed: {e:?}"))?;

                let mapped = slice.get_mapped_range();
                let row_pitch = dst_row_pitch_bytes as u64;
                for row in 0..mip_h as u64 {
                    let src_start = row as usize * padded_bpr as usize;
                    let src_end = src_start + unpadded_bpr as usize;
                    let dst_gpa = base_gpa
                        .checked_add(row.checked_mul(row_pitch).ok_or_else(|| {
                            anyhow!("COPY_TEXTURE2D: dst GPA overflow (row pitch mul)")
                        })?)
                        .ok_or_else(|| {
                            anyhow!("COPY_TEXTURE2D: dst GPA overflow (row pitch add)")
                        })?;
                    guest_mem
                        .write(dst_gpa, &mapped[src_start..src_end])
                        .map_err(anyhow_guest_mem)?;
                }
                drop(mapped);
                staging.unmap();
            }
        }

        // The destination GPU texture content has changed. For mip0/layer0 copies this means the
        // CPU shadow/dirty tracking for that subresource is now invalid; clear it so later uploads
        // don't overwrite GPU-produced pixels.
        //
        // For other subresources we currently keep `dirty`/`host_shadow` as-is since those track
        // mip0/layer0-only state.
        if let Some(dst) = self.resources.textures.get_mut(&dst_texture) {
            if dst_mip_level == 0 && dst_array_layer == 0 {
                dst.dirty = false;
                dst.host_shadow = None;
            }
        }

        Ok(())
    }

    fn exec_create_shader_dxbc(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, dxbc_bytes) = decode_cmd_create_shader_dxbc_payload_le(cmd_bytes)
            .map_err(|e| anyhow!("CREATE_SHADER_DXBC: invalid payload: {e:?}"))?;
        let shader_handle = cmd.shader_handle;
        let stage_u32 = cmd.stage;

        let stage = match stage_u32 {
            0 => ShaderStage::Vertex,
            1 => ShaderStage::Pixel,
            2 => ShaderStage::Compute,
            _ => bail!("CREATE_SHADER_DXBC: unknown shader stage {stage_u32}"),
        };

        let dxbc_hash_fnv1a64 = fnv1a64(dxbc_bytes);
        let dxbc = DxbcFile::parse(dxbc_bytes).context("DXBC parse failed")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("DXBC decode failed")?;
        let parsed_stage = match program.stage {
            crate::ShaderStage::Vertex => ShaderStage::Vertex,
            crate::ShaderStage::Pixel => ShaderStage::Pixel,
            crate::ShaderStage::Compute => ShaderStage::Compute,
            // Geometry/hull/domain stages are not represented in the AeroGPU command stream (WebGPU
            // does not expose them), but Win7 D3D11 applications may still create these shaders.
            //
            // Accept the create to keep the command stream robust, but ignore the shader since it
            // can never be bound (no GS/HS/DS slot in `AEROGPU_CMD_BIND_SHADERS`).
            crate::ShaderStage::Geometry | crate::ShaderStage::Hull | crate::ShaderStage::Domain => {
                return Ok(());
            }
            other => bail!("CREATE_SHADER_DXBC: unsupported DXBC shader stage {other:?}"),
        };
        if parsed_stage != stage {
            bail!("CREATE_SHADER_DXBC: stage mismatch (cmd={stage:?}, dxbc={parsed_stage:?})");
        }

        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;
        let translated = try_translate_sm4_signature_driven(&dxbc, &program, &signatures)?;
        let wgsl = translated.wgsl;
        let reflection = translated.reflection;

        let entry_point = match stage {
            ShaderStage::Vertex => "vs_main",
            ShaderStage::Pixel => "fs_main",
            ShaderStage::Compute => "cs_main",
        };

        let (hash, _module) = self.pipeline_cache.get_or_create_shader_module(
            &self.device,
            map_pipeline_cache_stage(stage),
            &wgsl,
            Some("aerogpu_cmd shader"),
        );

        let vs_input_signature = if stage == ShaderStage::Vertex {
            extract_vs_input_signature(&signatures).context("extract VS input signature")?
        } else {
            Vec::new()
        };

        #[cfg(debug_assertions)]
        let shader = ShaderResource {
            stage,
            wgsl_hash: hash,
            dxbc_hash_fnv1a64,
            entry_point,
            vs_input_signature,
            reflection,
            wgsl_source: wgsl,
        };
        #[cfg(not(debug_assertions))]
        let shader = ShaderResource {
            stage,
            wgsl_hash: hash,
            dxbc_hash_fnv1a64,
            entry_point,
            vs_input_signature,
            reflection,
        };

        self.resources.shaders.insert(shader_handle, shader);
        Ok(())
    }

    fn exec_destroy_shader(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_destroy_shader (16 bytes)
        if cmd_bytes.len() != 16 {
            bail!("DESTROY_SHADER: expected 16 bytes, got {}", cmd_bytes.len());
        }
        let shader_handle = read_u32_le(cmd_bytes, 8)?;
        self.resources.shaders.remove(&shader_handle);
        Ok(())
    }

    fn exec_bind_shaders(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_bind_shaders (24 bytes)
        if cmd_bytes.len() != 24 {
            bail!("BIND_SHADERS: expected 24 bytes, got {}", cmd_bytes.len());
        }
        let vs = read_u32_le(cmd_bytes, 8)?;
        let ps = read_u32_le(cmd_bytes, 12)?;
        let cs = read_u32_le(cmd_bytes, 16)?;

        self.state.vs = if vs == 0 { None } else { Some(vs) };
        self.state.ps = if ps == 0 { None } else { Some(ps) };
        self.state.cs = if cs == 0 { None } else { Some(cs) };
        Ok(())
    }

    fn exec_create_input_layout(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, blob) = decode_cmd_create_input_layout_blob_le(cmd_bytes)
            .map_err(|e| anyhow!("CREATE_INPUT_LAYOUT: invalid payload: {e:?}"))?;
        let handle = cmd.input_layout_handle;

        let layout = InputLayoutDesc::parse(blob)
            .map_err(|e| anyhow!("CREATE_INPUT_LAYOUT: failed to parse ILAY blob: {e}"))?;
        self.resources.input_layouts.insert(
            handle,
            InputLayoutResource {
                layout,
                mapping_cache: HashMap::new(),
            },
        );
        Ok(())
    }

    fn exec_destroy_input_layout(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        if cmd_bytes.len() != 16 {
            bail!(
                "DESTROY_INPUT_LAYOUT: expected 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        self.resources.input_layouts.remove(&handle);
        if self.state.input_layout == Some(handle) {
            self.state.input_layout = None;
        }
        Ok(())
    }

    fn exec_set_input_layout(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        if cmd_bytes.len() != 16 {
            bail!(
                "SET_INPUT_LAYOUT: expected 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        self.state.input_layout = if handle == 0 { None } else { Some(handle) };
        Ok(())
    }

    fn exec_set_render_targets(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_render_targets (48 bytes)
        if cmd_bytes.len() != 48 {
            bail!(
                "SET_RENDER_TARGETS: expected 48 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let color_count = read_u32_le(cmd_bytes, 8)? as usize;
        let depth_stencil = read_u32_le(cmd_bytes, 12)?;
        if color_count > 8 {
            bail!("SET_RENDER_TARGETS: color_count out of range: {color_count}");
        }
        let mut colors = Vec::with_capacity(color_count);
        let mut seen_gap = false;
        for i in 0..color_count {
            let tex_id = read_u32_le(cmd_bytes, 16 + i * 4)?;
            if tex_id == 0 {
                seen_gap = true;
                continue;
            }
            if seen_gap {
                bail!("SET_RENDER_TARGETS: render target slot {i} is set after an earlier slot was unbound (gaps are not supported yet)");
            }
            colors.push(tex_id);
        }
        self.state.render_targets = colors;
        self.state.depth_stencil = if depth_stencil == 0 {
            None
        } else {
            Some(depth_stencil)
        };
        Ok(())
    }

    fn exec_set_viewport(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_viewport (32 bytes)
        if cmd_bytes.len() != 32 {
            bail!("SET_VIEWPORT: expected 32 bytes, got {}", cmd_bytes.len());
        }
        let x = f32::from_bits(read_u32_le(cmd_bytes, 8)?);
        let y = f32::from_bits(read_u32_le(cmd_bytes, 12)?);
        let width = f32::from_bits(read_u32_le(cmd_bytes, 16)?);
        let height = f32::from_bits(read_u32_le(cmd_bytes, 20)?);
        let min_depth = f32::from_bits(read_u32_le(cmd_bytes, 24)?);
        let max_depth = f32::from_bits(read_u32_le(cmd_bytes, 28)?);
        self.state.viewport = Some(Viewport {
            x,
            y,
            width,
            height,
            min_depth,
            max_depth,
        });
        Ok(())
    }

    fn exec_set_scissor(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_scissor (24 bytes)
        if cmd_bytes.len() != 24 {
            bail!("SET_SCISSOR: expected 24 bytes, got {}", cmd_bytes.len());
        }
        let x = read_i32_le(cmd_bytes, 8)?;
        let y = read_i32_le(cmd_bytes, 12)?;
        let w = read_i32_le(cmd_bytes, 16)?;
        let h = read_i32_le(cmd_bytes, 20)?;
        if w <= 0 || h <= 0 {
            self.state.scissor = None;
            return Ok(());
        }
        let left = x.max(0);
        let top = y.max(0);
        let right = x.saturating_add(w).max(0);
        let bottom = y.saturating_add(h).max(0);
        if right <= left || bottom <= top {
            self.state.scissor = None;
            return Ok(());
        }
        self.state.scissor = Some(Scissor {
            x: left as u32,
            y: top as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        });
        Ok(())
    }

    fn exec_set_vertex_buffers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, bindings) = decode_cmd_set_vertex_buffers_bindings_le(cmd_bytes)
            .map_err(|e| anyhow!("SET_VERTEX_BUFFERS: invalid payload: {e:?}"))?;
        let start_slot = cmd.start_slot as usize;
        let buffer_count = cmd.buffer_count as usize;

        if start_slot + buffer_count > self.state.vertex_buffers.len() {
            bail!("SET_VERTEX_BUFFERS: slot range out of bounds");
        }

        for (i, binding) in bindings.iter().copied().enumerate() {
            let buffer = u32::from_le(binding.buffer);
            let stride_bytes = u32::from_le(binding.stride_bytes);
            let offset_bytes = u64::from(u32::from_le(binding.offset_bytes));

            self.state.vertex_buffers[start_slot + i] = if buffer == 0 {
                None
            } else {
                Some(VertexBufferBinding {
                    buffer,
                    stride_bytes,
                    offset_bytes,
                })
            };
        }
        Ok(())
    }

    fn exec_set_index_buffer(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_index_buffer (24 bytes)
        if cmd_bytes.len() != 24 {
            bail!(
                "SET_INDEX_BUFFER: expected 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let buffer = read_u32_le(cmd_bytes, 8)?;
        let format_u32 = read_u32_le(cmd_bytes, 12)?;
        let offset_bytes = read_u32_le(cmd_bytes, 16)? as u64;

        if buffer == 0 {
            self.state.index_buffer = None;
            return Ok(());
        }

        let format = match format_u32 {
            0 => wgpu::IndexFormat::Uint16,
            1 => wgpu::IndexFormat::Uint32,
            _ => bail!("SET_INDEX_BUFFER: unknown index format {format_u32}"),
        };
        self.state.index_buffer = Some(IndexBufferBinding {
            buffer,
            format,
            offset_bytes,
        });
        Ok(())
    }

    fn exec_set_primitive_topology(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_primitive_topology (16 bytes)
        if cmd_bytes.len() != 16 {
            bail!(
                "SET_PRIMITIVE_TOPOLOGY: expected 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let topology_u32 = read_u32_le(cmd_bytes, 8)?;
        self.state.primitive_topology = match topology_u32 {
            1 => wgpu::PrimitiveTopology::PointList,
            2 => wgpu::PrimitiveTopology::LineList,
            3 => wgpu::PrimitiveTopology::LineStrip,
            4 => wgpu::PrimitiveTopology::TriangleList,
            5 => wgpu::PrimitiveTopology::TriangleStrip,
            // TriangleFan is not directly supported; fall back to TriangleList.
            6 => wgpu::PrimitiveTopology::TriangleList,
            other => bail!("SET_PRIMITIVE_TOPOLOGY: unknown topology {other}"),
        };
        Ok(())
    }

    fn exec_set_texture(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_texture (24 bytes)
        if cmd_bytes.len() != 24 {
            bail!("SET_TEXTURE: expected 24 bytes, got {}", cmd_bytes.len());
        }
        let stage_u32 = read_u32_le(cmd_bytes, 8)?;
        let slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let texture = read_u32_le(cmd_bytes, 16)?;

        let slot: usize = slot_u32
            .try_into()
            .map_err(|_| anyhow!("SET_TEXTURE: slot out of range"))?;
        if slot >= DEFAULT_MAX_TEXTURE_SLOTS {
            bail!("SET_TEXTURE: slot out of supported range: {slot}");
        }
        let texture = if texture == 0 { None } else { Some(texture) };

        let slots = match stage_u32 {
            0 => &mut self.state.textures_vs,
            1 => &mut self.state.textures_ps,
            2 => &mut self.state.textures_cs,
            _ => bail!("SET_TEXTURE: unknown shader stage {stage_u32}"),
        };
        slots[slot] = texture;
        Ok(())
    }

    fn exec_set_sampler_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_sampler_state (24 bytes)
        if cmd_bytes.len() != 24 {
            bail!(
                "SET_SAMPLER_STATE: expected 24 bytes, got {}",
                cmd_bytes.len()
            );
        }

        // For now we ignore the D3D9 sampler-state details; the executor binds a
        // default sampler for all referenced slots.
        let _stage_u32 = read_u32_le(cmd_bytes, 8)?;
        let _slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let _state = read_u32_le(cmd_bytes, 16)?;
        let _value = read_u32_le(cmd_bytes, 20)?;
        Ok(())
    }

    fn exec_create_sampler(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_create_sampler (28 bytes)
        if cmd_bytes.len() != 28 {
            bail!("CREATE_SAMPLER: expected 28 bytes, got {}", cmd_bytes.len());
        }
        let sampler_handle = read_u32_le(cmd_bytes, 8)?;
        let filter_u32 = read_u32_le(cmd_bytes, 12)?;
        let address_u_u32 = read_u32_le(cmd_bytes, 16)?;
        let address_v_u32 = read_u32_le(cmd_bytes, 20)?;
        let address_w_u32 = read_u32_le(cmd_bytes, 24)?;

        if sampler_handle == 0 {
            bail!("CREATE_SAMPLER: sampler_handle must be non-zero");
        }

        let filter = map_sampler_filter(filter_u32)
            .ok_or_else(|| anyhow!("CREATE_SAMPLER: unknown filter {filter_u32}"))?;
        let address_u = map_sampler_address_mode(address_u_u32)
            .ok_or_else(|| anyhow!("CREATE_SAMPLER: unknown address_u {address_u_u32}"))?;
        let address_v = map_sampler_address_mode(address_v_u32)
            .ok_or_else(|| anyhow!("CREATE_SAMPLER: unknown address_v {address_v_u32}"))?;
        let address_w = map_sampler_address_mode(address_w_u32)
            .ok_or_else(|| anyhow!("CREATE_SAMPLER: unknown address_w {address_w_u32}"))?;

        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aerogpu_cmd sampler"),
            address_mode_u: address_u,
            address_mode_v: address_v,
            address_mode_w: address_w,
            mag_filter: filter,
            min_filter: filter,
            mipmap_filter: filter,
            ..Default::default()
        });

        #[cfg(debug_assertions)]
        let resource = SamplerResource {
            sampler,
            desc: AerogpuSamplerDesc {
                filter: filter_u32,
                address_u: address_u_u32,
                address_v: address_v_u32,
                address_w: address_w_u32,
            },
        };
        #[cfg(not(debug_assertions))]
        let resource = SamplerResource { sampler };

        self.resources.samplers.insert(sampler_handle, resource);
        Ok(())
    }

    fn exec_destroy_sampler(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_destroy_sampler (16 bytes)
        if cmd_bytes.len() != 16 {
            bail!(
                "DESTROY_SAMPLER: expected 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let sampler_handle = read_u32_le(cmd_bytes, 8)?;
        self.resources.samplers.remove(&sampler_handle);

        // Clean up bindings in state.
        for slots in [
            &mut self.state.samplers_vs,
            &mut self.state.samplers_ps,
            &mut self.state.samplers_cs,
        ] {
            for slot in slots {
                if *slot == Some(sampler_handle) {
                    *slot = None;
                }
            }
        }
        Ok(())
    }

    fn exec_set_samplers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_samplers (24 bytes) + aerogpu_handle_t samplers[sampler_count]
        if cmd_bytes.len() < 24 {
            bail!("SET_SAMPLERS: truncated packet");
        }
        let stage_u32 = read_u32_le(cmd_bytes, 8)?;
        let start_slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let sampler_count_u32 = read_u32_le(cmd_bytes, 16)?;

        let start_slot: usize = start_slot_u32
            .try_into()
            .map_err(|_| anyhow!("SET_SAMPLERS: start_slot out of range"))?;
        let sampler_count: usize = sampler_count_u32
            .try_into()
            .map_err(|_| anyhow!("SET_SAMPLERS: sampler_count out of range"))?;

        let payload_bytes = sampler_count
            .checked_mul(4)
            .ok_or_else(|| anyhow!("SET_SAMPLERS: sampler_count overflow"))?;
        let required = 24usize
            .checked_add(payload_bytes)
            .ok_or_else(|| anyhow!("SET_SAMPLERS: packet size overflow"))?;
        if cmd_bytes.len() < required {
            bail!(
                "SET_SAMPLERS: expected at least {required} bytes, got {}",
                cmd_bytes.len()
            );
        }

        let slots = match stage_u32 {
            0 => &mut self.state.samplers_vs,
            1 => &mut self.state.samplers_ps,
            2 => &mut self.state.samplers_cs,
            _ => bail!("SET_SAMPLERS: unknown shader stage {stage_u32}"),
        };

        let end_slot = start_slot
            .checked_add(sampler_count)
            .ok_or_else(|| anyhow!("SET_SAMPLERS: slot range overflow"))?;
        if end_slot > DEFAULT_MAX_SAMPLER_SLOTS {
            bail!(
                "SET_SAMPLERS: slot range out of supported range: start_slot={start_slot} sampler_count={sampler_count}"
            );
        }

        for i in 0..sampler_count {
            let handle = read_u32_le(cmd_bytes, 24 + i * 4)?;
            slots[start_slot + i] = if handle == 0 { None } else { Some(handle) };
        }

        Ok(())
    }

    fn exec_set_constant_buffers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_constant_buffers (24 bytes)
        // + aerogpu_constant_buffer_binding bindings[buffer_count] (16 bytes each)
        if cmd_bytes.len() < 24 {
            bail!("SET_CONSTANT_BUFFERS: truncated packet");
        }
        let stage_u32 = read_u32_le(cmd_bytes, 8)?;
        let start_slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let buffer_count_u32 = read_u32_le(cmd_bytes, 16)?;

        let start_slot: usize = start_slot_u32
            .try_into()
            .map_err(|_| anyhow!("SET_CONSTANT_BUFFERS: start_slot out of range"))?;
        let buffer_count: usize = buffer_count_u32
            .try_into()
            .map_err(|_| anyhow!("SET_CONSTANT_BUFFERS: buffer_count out of range"))?;

        let payload_bytes = buffer_count
            .checked_mul(16)
            .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: buffer_count overflow"))?;
        let required = 24usize
            .checked_add(payload_bytes)
            .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: packet size overflow"))?;
        if cmd_bytes.len() < required {
            bail!(
                "SET_CONSTANT_BUFFERS: expected at least {required} bytes, got {}",
                cmd_bytes.len()
            );
        }

        let slots = match stage_u32 {
            0 => &mut self.state.constant_buffers_vs,
            1 => &mut self.state.constant_buffers_ps,
            2 => &mut self.state.constant_buffers_cs,
            _ => bail!("SET_CONSTANT_BUFFERS: unknown shader stage {stage_u32}"),
        };

        let end_slot = start_slot
            .checked_add(buffer_count)
            .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: slot range overflow"))?;
        if end_slot > DEFAULT_MAX_CONSTANT_BUFFER_SLOTS {
            bail!(
                "SET_CONSTANT_BUFFERS: slot range out of supported range: start_slot={start_slot} buffer_count={buffer_count}"
            );
        }

        for i in 0..buffer_count {
            let base = 24 + i * 16;
            let buffer = read_u32_le(cmd_bytes, base)?;
            let offset_bytes = read_u32_le(cmd_bytes, base + 4)?;
            let size_bytes = read_u32_le(cmd_bytes, base + 8)?;
            // reserved0 @ +12 ignored.
            slots[start_slot + i] = if buffer == 0 {
                None
            } else {
                Some(ConstantBufferBinding {
                    buffer,
                    offset_bytes,
                    size_bytes,
                })
            };
        }

        Ok(())
    }

    fn exec_set_blend_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_blend_state (28 bytes minimum; extended in newer ABI versions).
        if cmd_bytes.len() < 28 {
            bail!(
                "SET_BLEND_STATE: expected at least 28 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let enable = read_u32_le(cmd_bytes, 8)? != 0;
        let src_factor = read_u32_le(cmd_bytes, 12)?;
        let dst_factor = read_u32_le(cmd_bytes, 16)?;
        let op = read_u32_le(cmd_bytes, 20)?;
        let write_mask = cmd_bytes[24];

        self.state.color_write_mask = map_color_write_mask(write_mask);

        // Optional extended fields (default when absent).
        let src_factor_alpha = if cmd_bytes.len() >= 32 {
            read_u32_le(cmd_bytes, 28)?
        } else {
            src_factor
        };
        let dst_factor_alpha = if cmd_bytes.len() >= 36 {
            read_u32_le(cmd_bytes, 32)?
        } else {
            dst_factor
        };
        let op_alpha = if cmd_bytes.len() >= 40 {
            read_u32_le(cmd_bytes, 36)?
        } else {
            op
        };

        let mut blend_constant = [0.0f32; 4];
        if cmd_bytes.len() >= 44 {
            blend_constant[0] = f32::from_bits(read_u32_le(cmd_bytes, 40)?);
        }
        if cmd_bytes.len() >= 48 {
            blend_constant[1] = f32::from_bits(read_u32_le(cmd_bytes, 44)?);
        }
        if cmd_bytes.len() >= 52 {
            blend_constant[2] = f32::from_bits(read_u32_le(cmd_bytes, 48)?);
        }
        if cmd_bytes.len() >= 56 {
            blend_constant[3] = f32::from_bits(read_u32_le(cmd_bytes, 52)?);
        }
        let sample_mask = if cmd_bytes.len() >= 60 {
            read_u32_le(cmd_bytes, 56)?
        } else {
            0xFFFF_FFFF
        };

        self.state.blend_constant = blend_constant;
        self.state.sample_mask = sample_mask;

        if !enable {
            self.state.blend = None;
            return Ok(());
        }

        let src = map_blend_factor(src_factor).unwrap_or(wgpu::BlendFactor::One);
        let dst = map_blend_factor(dst_factor).unwrap_or(wgpu::BlendFactor::Zero);
        let op = map_blend_op(op).unwrap_or(wgpu::BlendOperation::Add);

        let src_a = map_blend_factor(src_factor_alpha).unwrap_or(src);
        let dst_a = map_blend_factor(dst_factor_alpha).unwrap_or(dst);
        let op_a = map_blend_op(op_alpha).unwrap_or(op);

        self.state.blend = Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: src,
                dst_factor: dst,
                operation: op,
            },
            alpha: wgpu::BlendComponent {
                src_factor: src_a,
                dst_factor: dst_a,
                operation: op_a,
            },
        });
        Ok(())
    }

    fn exec_set_depth_stencil_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdSetDepthStencilState;

        // struct aerogpu_cmd_set_depth_stencil_state (28 bytes)
        if cmd_bytes.len() != std::mem::size_of::<AerogpuCmdSetDepthStencilState>() {
            bail!(
                "SET_DEPTH_STENCIL_STATE: expected {} bytes, got {}",
                std::mem::size_of::<AerogpuCmdSetDepthStencilState>(),
                cmd_bytes.len()
            );
        }

        let cmd: AerogpuCmdSetDepthStencilState = read_packed_unaligned(cmd_bytes)?;
        let state = cmd.state;

        let depth_enable = u32::from_le(state.depth_enable) != 0;
        let depth_write_enable = u32::from_le(state.depth_write_enable) != 0;
        let depth_func = u32::from_le(state.depth_func);
        let stencil_enable = u32::from_le(state.stencil_enable) != 0;

        let depth_compare = map_compare_func(depth_func).unwrap_or(wgpu::CompareFunction::Always);

        self.state.depth_enable = depth_enable;
        self.state.depth_write_enable = depth_enable && depth_write_enable;
        self.state.depth_compare = if depth_enable {
            depth_compare
        } else {
            wgpu::CompareFunction::Always
        };
        self.state.stencil_enable = stencil_enable;
        self.state.stencil_read_mask = state.stencil_read_mask;
        self.state.stencil_write_mask = state.stencil_write_mask;
        Ok(())
    }

    fn exec_set_rasterizer_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdSetRasterizerState;

        // struct aerogpu_cmd_set_rasterizer_state (32 bytes)
        if cmd_bytes.len() != std::mem::size_of::<AerogpuCmdSetRasterizerState>() {
            bail!(
                "SET_RASTERIZER_STATE: expected {} bytes, got {}",
                std::mem::size_of::<AerogpuCmdSetRasterizerState>(),
                cmd_bytes.len()
            );
        }
        let cmd: AerogpuCmdSetRasterizerState = read_packed_unaligned(cmd_bytes)?;
        let state = cmd.state;

        let cull_mode = u32::from_le(state.cull_mode);
        let front_ccw = u32::from_le(state.front_ccw) != 0;
        let scissor_enable = u32::from_le(state.scissor_enable) != 0;
        let depth_bias = i32::from_le(state.depth_bias);

        self.state.cull_mode = match cull_mode {
            0 => None,
            1 => Some(wgpu::Face::Front),
            2 => Some(wgpu::Face::Back),
            _ => self.state.cull_mode,
        };
        self.state.front_face = if front_ccw {
            wgpu::FrontFace::Ccw
        } else {
            wgpu::FrontFace::Cw
        };
        self.state.scissor_enable = scissor_enable;
        self.state.depth_bias = depth_bias;
        Ok(())
    }

    fn exec_clear(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &dyn GuestMemory,
    ) -> Result<()> {
        // struct aerogpu_cmd_clear (36 bytes)
        if cmd_bytes.len() != 36 {
            bail!("CLEAR: expected 36 bytes, got {}", cmd_bytes.len());
        }
        if self.state.render_targets.is_empty() && self.state.depth_stencil.is_none() {
            // Nothing bound; treat as no-op for robustness.
            return Ok(());
        }

        let render_targets = self.state.render_targets.clone();
        let depth_stencil = self.state.depth_stencil;
        for &handle in &render_targets {
            self.ensure_texture_uploaded(handle, allocs, guest_mem)?;
        }
        if let Some(handle) = depth_stencil {
            self.ensure_texture_uploaded(handle, allocs, guest_mem)?;
        }

        let flags = read_u32_le(cmd_bytes, 8)?;
        let color = [
            f32::from_bits(read_u32_le(cmd_bytes, 12)?),
            f32::from_bits(read_u32_le(cmd_bytes, 16)?),
            f32::from_bits(read_u32_le(cmd_bytes, 20)?),
            f32::from_bits(read_u32_le(cmd_bytes, 24)?),
        ];
        let depth = f32::from_bits(read_u32_le(cmd_bytes, 28)?);
        let stencil = read_u32_le(cmd_bytes, 32)? as u32;

        // Clear writes modify the underlying textures; invalidate any CPU shadows.
        if flags & AEROGPU_CLEAR_COLOR != 0 {
            for &handle in &render_targets {
                if let Some(tex) = self.resources.textures.get_mut(&handle) {
                    tex.host_shadow = None;
                }
            }
        }
        if (flags & (AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL)) != 0 {
            if let Some(handle) = depth_stencil {
                if let Some(tex) = self.resources.textures.get_mut(&handle) {
                    tex.host_shadow = None;
                }
            }
        }

        let (mut color_attachments, mut depth_stencil_attachment) =
            build_render_pass_attachments(&self.resources, &self.state, wgpu::LoadOp::Load)?;

        if flags & AEROGPU_CLEAR_COLOR != 0 {
            for att in &mut color_attachments {
                if let Some(att) = att.as_mut() {
                    att.ops.load = wgpu::LoadOp::Clear(wgpu::Color {
                        r: color[0] as f64,
                        g: color[1] as f64,
                        b: color[2] as f64,
                        a: color[3] as f64,
                    });
                }
            }
        }

        if let Some(ds) = depth_stencil_attachment.as_mut() {
            if flags & AEROGPU_CLEAR_DEPTH != 0 {
                if let Some(depth_ops) = ds.depth_ops.as_mut() {
                    depth_ops.load = wgpu::LoadOp::Clear(depth);
                }
            }
            if flags & AEROGPU_CLEAR_STENCIL != 0 {
                if let Some(stencil_ops) = ds.stencil_ops.as_mut() {
                    stencil_ops.load = wgpu::LoadOp::Clear(stencil);
                }
            }
        }

        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu_cmd clear pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        Ok(())
    }

    fn exec_present(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        report: &mut ExecuteReport,
    ) -> Result<()> {
        // struct aerogpu_cmd_present (16 bytes)
        if cmd_bytes.len() != 16 {
            bail!("PRESENT: expected 16 bytes, got {}", cmd_bytes.len());
        }
        let scanout_id = read_u32_le(cmd_bytes, 8)?;
        let flags = read_u32_le(cmd_bytes, 12)?;
        let presented_render_target = self.state.render_targets.first().copied();
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: None,
            presented_render_target,
        });
        let new_encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu_cmd encoder after present"),
            });
        let finished = std::mem::replace(encoder, new_encoder).finish();
        self.queue.submit([finished]);
        Ok(())
    }

    fn exec_present_ex(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        report: &mut ExecuteReport,
    ) -> Result<()> {
        // struct aerogpu_cmd_present_ex (24 bytes)
        if cmd_bytes.len() != 24 {
            bail!("PRESENT_EX: expected 24 bytes, got {}", cmd_bytes.len());
        }
        let scanout_id = read_u32_le(cmd_bytes, 8)?;
        let flags = read_u32_le(cmd_bytes, 12)?;
        let d3d9_present_flags = read_u32_le(cmd_bytes, 16)?;
        let presented_render_target = self.state.render_targets.first().copied();
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: Some(d3d9_present_flags),
            presented_render_target,
        });
        let new_encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu_cmd encoder after present_ex"),
            });
        let finished = std::mem::replace(encoder, new_encoder).finish();
        self.queue.submit([finished]);
        Ok(())
    }

    fn exec_flush(&mut self, encoder: &mut wgpu::CommandEncoder) -> Result<()> {
        let new_encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu_cmd encoder after flush"),
            });
        let finished = std::mem::replace(encoder, new_encoder).finish();
        self.queue.submit([finished]);
        Ok(())
    }

    fn ensure_buffer_uploaded(
        &mut self,
        buffer_handle: u32,
        allocs: &AllocTable,
        guest_mem: &dyn GuestMemory,
    ) -> Result<()> {
        let Some(buf) = self.resources.buffers.get_mut(&buffer_handle) else {
            return Ok(());
        };
        let Some(dirty) = buf.dirty.take() else {
            return Ok(());
        };
        let Some(backing) = buf.backing else {
            return Ok(());
        };

        let dirty_len = dirty.end.saturating_sub(dirty.start);
        allocs.validate_range(
            backing.alloc_id,
            backing.offset_bytes + dirty.start,
            dirty_len,
        )?;
        let gpa = allocs.gpa(backing.alloc_id)? + backing.offset_bytes + dirty.start;

        // Upload in chunks to avoid allocating massive temporary buffers for big resources.
        const CHUNK: usize = 64 * 1024;
        if dirty.start % wgpu::COPY_BUFFER_ALIGNMENT != 0 {
            bail!(
                "buffer {buffer_handle} dirty range start {} does not respect COPY_BUFFER_ALIGNMENT",
                dirty.start
            );
        }
        let mut offset = dirty.start;
        while offset < dirty.end {
            let remaining = (dirty.end - offset) as usize;
            let n = remaining.min(CHUNK);
            let mut tmp = vec![0u8; n];
            guest_mem
                .read(gpa + (offset - dirty.start) as u64, &mut tmp)
                .map_err(|e| anyhow_guest_mem(e))?;

            let write_len = if n % (wgpu::COPY_BUFFER_ALIGNMENT as usize) != 0 {
                if offset + n as u64 != dirty.end || dirty.end != buf.size {
                    bail!("buffer {buffer_handle} upload is not COPY_BUFFER_ALIGNMENT-aligned");
                }
                let padded = align4(n);
                tmp.resize(padded, 0);
                padded
            } else {
                n
            };

            let end = offset
                .checked_add(write_len as u64)
                .ok_or_else(|| anyhow!("buffer upload range overflows u64"))?;
            if end > buf.gpu_size {
                bail!("buffer upload overruns wgpu buffer allocation");
            }

            self.queue
                .write_buffer(&buf.buffer, offset, &tmp[..write_len]);
            offset += n as u64;
        }

        Ok(())
    }

    fn ensure_texture_uploaded(
        &mut self,
        texture_handle: u32,
        allocs: &AllocTable,
        guest_mem: &dyn GuestMemory,
    ) -> Result<()> {
        let Some(tex) = self.resources.textures.get_mut(&texture_handle) else {
            return Ok(());
        };
        if !tex.dirty {
            return Ok(());
        }

        let Some(backing) = tex.backing else {
            tex.dirty = false;
            return Ok(());
        };

        let bytes_per_row = if tex.row_pitch_bytes != 0 {
            tex.row_pitch_bytes
        } else {
            tex.desc
                .width
                .checked_mul(bytes_per_texel(tex.desc.format)?)
                .ok_or_else(|| anyhow!("texture upload bytes_per_row overflow"))?
        };
        let total_size = (bytes_per_row as u64)
            .checked_mul(tex.desc.height as u64)
            .ok_or_else(|| anyhow!("texture upload size overflow"))?;
        allocs.validate_range(backing.alloc_id, backing.offset_bytes, total_size)?;
        let gpa = allocs.gpa(backing.alloc_id)? + backing.offset_bytes;

        // Avoid allocating `bytes_per_row * height` (and potentially a second repack buffer) for
        // large textures. We upload in row chunks, repacking only when required by WebGPU's
        // `COPY_BYTES_PER_ROW_ALIGNMENT`.
        const CHUNK_BYTES: usize = 256 * 1024;

        let bpt = bytes_per_texel(tex.desc.format)?;
        let unpadded_bpr = tex
            .desc
            .width
            .checked_mul(bpt)
            .ok_or_else(|| anyhow!("texture upload bytes_per_row overflow"))?;
        if bytes_per_row < unpadded_bpr {
            bail!("texture upload bytes_per_row too small");
        }

        let aligned = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let height_usize: usize = tex
            .desc
            .height
            .try_into()
            .map_err(|_| anyhow!("texture upload height out of range"))?;
        let src_row_pitch = bytes_per_row as usize;

        if tex.desc.height > 1 && bytes_per_row % aligned != 0 {
            // Repack each chunk into an aligned row pitch.
            let padded_bpr = ((unpadded_bpr + aligned - 1) / aligned) * aligned;
            let padded_bpr_usize = padded_bpr as usize;
            let rows_per_chunk = (CHUNK_BYTES / padded_bpr_usize).max(1);

            let mut row_buf = vec![0u8; unpadded_bpr as usize];
            for y0 in (0..height_usize).step_by(rows_per_chunk) {
                let rows = (height_usize - y0).min(rows_per_chunk);
                let mut repacked = vec![0u8; padded_bpr_usize * rows];
                for row in 0..rows {
                    let src_addr = gpa
                        .checked_add(((y0 + row) * src_row_pitch) as u64)
                        .ok_or_else(|| anyhow!("texture upload address overflows u64"))?;
                    guest_mem
                        .read(src_addr, &mut row_buf)
                        .map_err(anyhow_guest_mem)?;
                    let dst_start = row * padded_bpr_usize;
                    repacked[dst_start..dst_start + row_buf.len()].copy_from_slice(&row_buf);
                }

                self.queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &tex.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: y0 as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &repacked,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bpr),
                        rows_per_image: Some(rows as u32),
                    },
                    wgpu::Extent3d {
                        width: tex.desc.width,
                        height: rows as u32,
                        depth_or_array_layers: 1,
                    },
                );
            }
        } else {
            // `bytes_per_row` is already aligned (or the copy is a single row). Upload contiguous
            // chunks directly from guest memory.
            let rows_per_chunk = (CHUNK_BYTES / src_row_pitch).max(1);
            let mut tmp = vec![0u8; src_row_pitch * rows_per_chunk];
            for y0 in (0..height_usize).step_by(rows_per_chunk) {
                let rows = (height_usize - y0).min(rows_per_chunk);
                let byte_len = src_row_pitch
                    .checked_mul(rows)
                    .ok_or_else(|| anyhow!("texture upload chunk overflows usize"))?;
                let tmp_slice = &mut tmp[..byte_len];
                let src_addr = gpa
                    .checked_add((y0 * src_row_pitch) as u64)
                    .ok_or_else(|| anyhow!("texture upload address overflows u64"))?;
                guest_mem
                    .read(src_addr, tmp_slice)
                    .map_err(anyhow_guest_mem)?;

                self.queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &tex.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: y0 as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    tmp_slice,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(rows as u32),
                    },
                    wgpu::Extent3d {
                        width: tex.desc.width,
                        height: rows as u32,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        tex.dirty = false;
        Ok(())
    }

    fn prepare_pipeline_bindings(
        &mut self,
        vs: &ShaderReflection,
        ps: &ShaderReflection,
    ) -> Result<PreparedPipelineBindings> {
        let bindings = merge_shader_bindings([&vs.bindings, &ps.bindings])?;

        let max_group = bindings.iter().map(|b| b.group).max();
        let mut group_entries: Vec<Vec<wgpu::BindGroupLayoutEntry>> = match max_group {
            Some(max) => vec![Vec::new(); (max + 1) as usize],
            None => Vec::new(),
        };

        for b in &bindings {
            let entry = bind_group_layout_entry_for_binding(b)?;
            let group: usize = b
                .group
                .try_into()
                .map_err(|_| anyhow!("binding group out of range"))?;
            group_entries[group].push(entry);
        }

        let group_layouts: Vec<CachedBindGroupLayout> = group_entries
            .iter()
            .map(|entries| {
                self.bind_group_layout_cache
                    .get_or_create(&self.device, entries)
            })
            .collect();

        let layout_key = PipelineLayoutKey {
            bind_group_layout_hashes: group_layouts.iter().map(|l| l.hash).collect(),
        };

        let pipeline_layout = if let Some(existing) = self.pipeline_layout_cache.get(&layout_key) {
            existing.clone()
        } else {
            let bgl_refs: Vec<&wgpu::BindGroupLayout> =
                group_layouts.iter().map(|l| l.layout.as_ref()).collect();
            let pipeline_layout = Arc::new(self.device.create_pipeline_layout(
                &wgpu::PipelineLayoutDescriptor {
                    label: Some("aerogpu_cmd pipeline layout"),
                    bind_group_layouts: &bgl_refs,
                    push_constant_ranges: &[],
                },
            ));
            self.pipeline_layout_cache
                .insert(layout_key.clone(), pipeline_layout.clone());
            pipeline_layout
        };

        Ok(PreparedPipelineBindings {
            bindings,
            group_layouts,
            layout_key,
            pipeline_layout,
        })
    }
}

#[derive(Debug, Clone)]
struct PreparedPipelineBindings {
    bindings: Vec<Binding>,
    group_layouts: Vec<CachedBindGroupLayout>,
    layout_key: PipelineLayoutKey,
    pipeline_layout: Arc<wgpu::PipelineLayout>,
}

fn build_render_pass_attachments<'a>(
    resources: &'a AerogpuD3d11Resources,
    state: &'a AerogpuD3d11State,
    color_load: wgpu::LoadOp<wgpu::Color>,
) -> Result<(
    Vec<Option<wgpu::RenderPassColorAttachment<'a>>>,
    Option<wgpu::RenderPassDepthStencilAttachment<'a>>,
)> {
    let mut color_attachments = Vec::with_capacity(state.render_targets.len());
    for &tex_id in &state.render_targets {
        let tex = resources
            .textures
            .get(&tex_id)
            .ok_or_else(|| anyhow!("unknown render target texture {tex_id}"))?;
        color_attachments.push(Some(wgpu::RenderPassColorAttachment {
            view: &tex.view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: color_load,
                store: wgpu::StoreOp::Store,
            },
        }));
    }

    let depth_stencil_attachment = state.depth_stencil.and_then(|ds_id| {
        resources.textures.get(&ds_id).map(|tex| {
            let format = tex.desc.format;
            wgpu::RenderPassDepthStencilAttachment {
                view: &tex.view,
                depth_ops: texture_format_has_depth(format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: texture_format_has_stencil(format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
            }
        })
    });

    Ok((color_attachments, depth_stencil_attachment))
}

#[derive(Debug, Clone)]
struct BuiltVertexState {
    vertex_buffers: Vec<VertexBufferLayoutOwned>,
    vertex_buffer_keys: Vec<aero_gpu::pipeline_key::VertexBufferLayoutKey>,
    /// WebGPU vertex buffer slot → D3D11 input slot.
    wgpu_slot_to_d3d_slot: Vec<u32>,
}

fn exec_draw<'a>(pass: &mut wgpu::RenderPass<'a>, cmd_bytes: &[u8]) -> Result<()> {
    // struct aerogpu_cmd_draw (24 bytes)
    if cmd_bytes.len() != 24 {
        bail!("DRAW: expected 24 bytes, got {}", cmd_bytes.len());
    }
    let vertex_count = read_u32_le(cmd_bytes, 8)?;
    let instance_count = read_u32_le(cmd_bytes, 12)?;
    let first_vertex = read_u32_le(cmd_bytes, 16)?;
    let first_instance = read_u32_le(cmd_bytes, 20)?;
    pass.draw(
        first_vertex..first_vertex.saturating_add(vertex_count),
        first_instance..first_instance.saturating_add(instance_count),
    );
    Ok(())
}

fn exec_draw_indexed<'a>(pass: &mut wgpu::RenderPass<'a>, cmd_bytes: &[u8]) -> Result<()> {
    // struct aerogpu_cmd_draw_indexed (28 bytes)
    if cmd_bytes.len() != 28 {
        bail!("DRAW_INDEXED: expected 28 bytes, got {}", cmd_bytes.len());
    }
    let index_count = read_u32_le(cmd_bytes, 8)?;
    let instance_count = read_u32_le(cmd_bytes, 12)?;
    let first_index = read_u32_le(cmd_bytes, 16)?;
    let base_vertex = read_i32_le(cmd_bytes, 20)?;
    let first_instance = read_u32_le(cmd_bytes, 24)?;
    pass.draw_indexed(
        first_index..first_index.saturating_add(index_count),
        base_vertex,
        first_instance..first_instance.saturating_add(instance_count),
    );
    Ok(())
}

fn merge_shader_bindings(all: [&[Binding]; 2]) -> Result<Vec<Binding>> {
    use std::collections::hash_map::Entry;

    let mut merged: HashMap<(u32, u32), Binding> = HashMap::new();
    for list in all {
        for b in list {
            let key = (b.group, b.binding);
            match merged.entry(key) {
                Entry::Vacant(v) => {
                    v.insert(b.clone());
                }
                Entry::Occupied(mut o) => {
                    let existing = o.get_mut();
                    if existing.kind != b.kind {
                        bail!(
                            "binding kind mismatch for @group({}) @binding({}): existing={:?} new={:?}",
                            b.group,
                            b.binding,
                            existing.kind,
                            b.kind
                        );
                    }
                    existing.visibility |= b.visibility;
                }
            }
        }
    }

    let mut out: Vec<Binding> = merged.into_values().collect();
    out.sort_by_key(|b| (b.group, b.binding));
    Ok(out)
}

fn bind_group_layout_entry_for_binding(binding: &Binding) -> Result<wgpu::BindGroupLayoutEntry> {
    let ty = match &binding.kind {
        BindingKind::ConstantBuffer { reg_count, .. } => {
            let min_size = (*reg_count as u64).saturating_mul(16);
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(min_size),
            }
        }
        BindingKind::Texture2D { .. } => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        BindingKind::Sampler { .. } => {
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
        }
    };

    Ok(wgpu::BindGroupLayoutEntry {
        binding: binding.binding,
        visibility: binding.visibility,
        ty,
        count: None,
    })
}

fn resolve_texture_binding(state: &AerogpuD3d11State, binding: &Binding) -> Option<u32> {
    let slot: usize = binding.binding.try_into().ok()?;
    let vs = state.textures_vs.get(slot).and_then(|v| *v);
    let ps = state.textures_ps.get(slot).and_then(|v| *v);
    let cs = state.textures_cs.get(slot).and_then(|v| *v);

    if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
        if ps.is_some() {
            return ps;
        }
        if binding.visibility.contains(wgpu::ShaderStages::VERTEX) && vs.is_some() {
            return vs;
        }
    }
    if binding.visibility.contains(wgpu::ShaderStages::VERTEX) && vs.is_some() {
        return vs;
    }
    if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) && cs.is_some() {
        return cs;
    }
    None
}

fn resolve_sampler_binding(state: &AerogpuD3d11State, binding: &Binding) -> Option<u32> {
    let BindingKind::Sampler { slot } = binding.kind else {
        return None;
    };
    let slot: usize = slot.try_into().ok()?;
    let vs = state.samplers_vs.get(slot).and_then(|v| *v);
    let ps = state.samplers_ps.get(slot).and_then(|v| *v);
    let cs = state.samplers_cs.get(slot).and_then(|v| *v);

    if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
        if ps.is_some() {
            return ps;
        }
        if binding.visibility.contains(wgpu::ShaderStages::VERTEX) && vs.is_some() {
            return vs;
        }
    }
    if binding.visibility.contains(wgpu::ShaderStages::VERTEX) && vs.is_some() {
        return vs;
    }
    if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) && cs.is_some() {
        return cs;
    }
    None
}

fn resolve_constant_buffer_binding(
    state: &AerogpuD3d11State,
    binding: &Binding,
) -> Option<ConstantBufferBinding> {
    let BindingKind::ConstantBuffer { slot, .. } = binding.kind else {
        return None;
    };
    let slot: usize = slot.try_into().ok()?;
    let vs = state.constant_buffers_vs.get(slot).and_then(|v| *v);
    let ps = state.constant_buffers_ps.get(slot).and_then(|v| *v);
    let cs = state.constant_buffers_cs.get(slot).and_then(|v| *v);

    if binding.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
        if ps.is_some() {
            return ps;
        }
        if binding.visibility.contains(wgpu::ShaderStages::VERTEX) && vs.is_some() {
            return vs;
        }
    }
    if binding.visibility.contains(wgpu::ShaderStages::VERTEX) && vs.is_some() {
        return vs;
    }
    if binding.visibility.contains(wgpu::ShaderStages::COMPUTE) && cs.is_some() {
        return cs;
    }
    None
}

fn build_bind_groups(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    bindings: &[Binding],
    group_layouts: &[CachedBindGroupLayout],
    state: &AerogpuD3d11State,
    resources: &AerogpuD3d11Resources,
    fallback_texture_view: &wgpu::TextureView,
    fallback_sampler: &wgpu::Sampler,
    fallback_uniform_buffer: &wgpu::Buffer,
) -> Result<Vec<wgpu::BindGroup>> {
    let mut out = Vec::with_capacity(group_layouts.len());
    for (group, cached_layout) in group_layouts.iter().enumerate() {
        let group_u32 = group as u32;
        let group_bindings: Vec<&Binding> =
            bindings.iter().filter(|b| b.group == group_u32).collect();

        // Some constant-buffer bindings require an aligned staging buffer (WebGPU enforces a higher
        // offset alignment than D3D11). Build them up-front so we can safely reference them when
        // assembling `BindGroupEntry`s.
        let mut cb_scratch: Vec<wgpu::Buffer> = Vec::new();
        let mut cb_scratch_map: HashMap<u32, (usize, u64)> = HashMap::new();
        let uniform_align = device.limits().min_uniform_buffer_offset_alignment as u64;
        let max_uniform_binding_size = device.limits().max_uniform_buffer_binding_size as u64;

        for b in &group_bindings {
            let BindingKind::ConstantBuffer { reg_count, .. } = b.kind else {
                continue;
            };
            let Some(cb) = resolve_constant_buffer_binding(state, b) else {
                continue;
            };
            let Some(src) = resources.buffers.get(&cb.buffer) else {
                continue;
            };

            let offset = cb.offset_bytes as u64;
            if offset >= src.size {
                continue;
            }
            let mut size = cb.size_bytes as u64;
            if size == 0 {
                size = src.size - offset;
            }
            size = size.min(src.size - offset);
            let required_min = (reg_count as u64).saturating_mul(16);
            if size < required_min || size > max_uniform_binding_size {
                continue;
            }

            if offset % uniform_align == 0 {
                continue;
            }

            let scratch = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu_cmd constant buffer scratch"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(&src.buffer, offset, &scratch, 0, size);
            cb_scratch_map.insert(b.binding, (cb_scratch.len(), size));
            cb_scratch.push(scratch);
        }

        let mut entries: Vec<wgpu::BindGroupEntry<'_>> = Vec::new();
        for b in &group_bindings {
            let resource = match &b.kind {
                BindingKind::ConstantBuffer { reg_count, .. } => {
                    let required_min = (*reg_count as u64).saturating_mul(16);
                    if let Some((idx, size)) = cb_scratch_map.get(&b.binding).copied() {
                        let scratch = &cb_scratch[idx];
                        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: scratch,
                            offset: 0,
                            size: NonZeroU64::new(size),
                        })
                    } else {
                        match resolve_constant_buffer_binding(state, b) {
                            None => fallback_uniform_buffer.as_entire_binding(),
                            Some(cb) => match resources.buffers.get(&cb.buffer) {
                                None => fallback_uniform_buffer.as_entire_binding(),
                                Some(src) => {
                                    let offset = cb.offset_bytes as u64;
                                    if offset >= src.size {
                                        fallback_uniform_buffer.as_entire_binding()
                                    } else {
                                        let mut size = cb.size_bytes as u64;
                                        if size == 0 {
                                            size = src.size - offset;
                                        }
                                        size = size.min(src.size - offset);

                                        if size < required_min {
                                            fallback_uniform_buffer.as_entire_binding()
                                        } else if size > max_uniform_binding_size {
                                            // Can't bind this range directly. If the whole buffer
                                            // fits, fall back to binding it at offset 0 (ignoring
                                            // range).
                                            if src.size <= max_uniform_binding_size
                                                && required_min <= src.size
                                            {
                                                src.buffer.as_entire_binding()
                                            } else {
                                                fallback_uniform_buffer.as_entire_binding()
                                            }
                                        } else if offset % uniform_align == 0 {
                                            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                                buffer: &src.buffer,
                                                offset,
                                                size: NonZeroU64::new(size),
                                            })
                                        } else if offset == 0 {
                                            src.buffer.as_entire_binding()
                                        } else {
                                            // Unaligned offsets should have been handled by the
                                            // scratch path. Fall back to binding the whole buffer
                                            // when possible.
                                            if src.size <= max_uniform_binding_size
                                                && required_min <= src.size
                                            {
                                                src.buffer.as_entire_binding()
                                            } else {
                                                fallback_uniform_buffer.as_entire_binding()
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }
                }
                BindingKind::Texture2D { .. } => {
                    let handle = resolve_texture_binding(state, b);
                    let view = handle
                        .and_then(|h| resources.textures.get(&h).map(|t| &t.view))
                        .unwrap_or(fallback_texture_view);
                    wgpu::BindingResource::TextureView(view)
                }
                BindingKind::Sampler { .. } => {
                    let handle = resolve_sampler_binding(state, b);
                    let sampler = handle
                        .and_then(|h| resources.samplers.get(&h).map(|s| &s.sampler))
                        .unwrap_or(fallback_sampler);
                    wgpu::BindingResource::Sampler(sampler)
                }
            };
            entries.push(wgpu::BindGroupEntry {
                binding: b.binding,
                resource,
            });
        }
        entries.sort_by_key(|e| e.binding);
        out.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu_cmd bind group"),
            layout: cached_layout.layout.as_ref(),
            entries: &entries,
        }));
    }
    Ok(out)
}

fn get_or_create_render_pipeline_for_state<'a>(
    device: &wgpu::Device,
    pipeline_cache: &'a mut PipelineCache,
    pipeline_layout: &wgpu::PipelineLayout,
    resources: &mut AerogpuD3d11Resources,
    state: &AerogpuD3d11State,
    layout_key: PipelineLayoutKey,
) -> Result<(RenderPipelineKey, &'a wgpu::RenderPipeline, Vec<u32>)> {
    let vs_handle = state
        .vs
        .ok_or_else(|| anyhow!("render draw without bound VS"))?;
    let ps_handle = state
        .ps
        .ok_or_else(|| anyhow!("render draw without bound PS"))?;
    let (vs_wgsl_hash, vs_dxbc_hash_fnv1a64, vs_entry_point, vs_input_signature) = {
        let vs = resources
            .shaders
            .get(&vs_handle)
            .ok_or_else(|| anyhow!("unknown VS shader {vs_handle}"))?;
        if vs.stage != ShaderStage::Vertex {
            bail!("shader {vs_handle} is not a vertex shader");
        }
        (
            vs.wgsl_hash,
            vs.dxbc_hash_fnv1a64,
            vs.entry_point,
            vs.vs_input_signature.clone(),
        )
    };
    let (ps_wgsl_hash, fs_entry_point) = {
        let ps = resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?;
        if ps.stage != ShaderStage::Pixel {
            bail!("shader {ps_handle} is not a pixel shader");
        }
        (ps.wgsl_hash, ps.entry_point)
    };

    let BuiltVertexState {
        vertex_buffers,
        vertex_buffer_keys,
        wgpu_slot_to_d3d_slot,
    } = build_vertex_buffers_for_pipeline(
        resources,
        state,
        vs_dxbc_hash_fnv1a64,
        &vs_input_signature,
    )?;

    let mut color_targets = Vec::with_capacity(state.render_targets.len());
    let mut color_target_states = Vec::with_capacity(state.render_targets.len());
    for &rt in &state.render_targets {
        let tex = resources
            .textures
            .get(&rt)
            .ok_or_else(|| anyhow!("unknown render target texture {rt}"))?;
        let ct = wgpu::ColorTargetState {
            format: tex.desc.format,
            blend: state.blend,
            write_mask: state.color_write_mask,
        };
        color_targets.push(ColorTargetKey {
            format: ct.format,
            blend: ct.blend.map(Into::into),
            write_mask: ct.write_mask,
        });
        color_target_states.push(Some(ct));
    }

    let depth_stencil_state = if let Some(ds_id) = state.depth_stencil {
        let tex = resources
            .textures
            .get(&ds_id)
            .ok_or_else(|| anyhow!("unknown depth-stencil texture {ds_id}"))?;
        let format = tex.desc.format;
        if !texture_format_has_depth(format) {
            bail!(
                "depth-stencil texture {ds_id} has non-depth format {:?}",
                format
            );
        }

        let depth_compare = if state.depth_enable {
            state.depth_compare
        } else {
            wgpu::CompareFunction::Always
        };
        let depth_write_enabled = state.depth_enable && state.depth_write_enable;

        let (read_mask, write_mask) = if texture_format_has_stencil(format) && state.stencil_enable
        {
            (
                state.stencil_read_mask as u32,
                state.stencil_write_mask as u32,
            )
        } else {
            (0, 0)
        };

        Some(wgpu::DepthStencilState {
            format,
            depth_write_enabled,
            depth_compare,
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState::IGNORE,
                back: wgpu::StencilFaceState::IGNORE,
                read_mask,
                write_mask,
            },
            bias: wgpu::DepthBiasState {
                constant: state.depth_bias,
                slope_scale: 0.0,
                clamp: 0.0,
            },
        })
    } else {
        None
    };
    let depth_stencil_key = depth_stencil_state.as_ref().map(|ds| ds.clone().into());

    let key = RenderPipelineKey {
        vertex_shader: vs_wgsl_hash,
        fragment_shader: ps_wgsl_hash,
        color_targets,
        depth_stencil: depth_stencil_key,
        primitive_topology: state.primitive_topology,
        cull_mode: state.cull_mode,
        front_face: state.front_face,
        scissor_enabled: state.scissor_enable,
        vertex_buffers: vertex_buffer_keys,
        sample_count: 1,
        layout: layout_key,
    };

    let topology = state.primitive_topology;
    let cull_mode = state.cull_mode;
    let front_face = state.front_face;
    let depth_stencil_state_for_pipeline = depth_stencil_state.clone();

    let pipeline = pipeline_cache
        .get_or_create_render_pipeline(device, key.clone(), move |device, vs, fs| {
            let vb_layouts: Vec<wgpu::VertexBufferLayout<'_>> = vertex_buffers
                .iter()
                .map(VertexBufferLayoutOwned::as_wgpu)
                .collect();

            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu_cmd render pipeline"),
                layout: Some(pipeline_layout),
                vertex: wgpu::VertexState {
                    module: vs,
                    entry_point: vs_entry_point,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &vb_layouts,
                },
                fragment: Some(wgpu::FragmentState {
                    module: fs,
                    entry_point: fs_entry_point,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &color_target_states,
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    strip_index_format: None,
                    front_face,
                    cull_mode,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: depth_stencil_state_for_pipeline,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            })
        })
        .map_err(|e| anyhow!("wgpu pipeline cache: {e:?}"))?;

    Ok((key, pipeline, wgpu_slot_to_d3d_slot))
}

fn build_vertex_buffers_for_pipeline(
    resources: &mut AerogpuD3d11Resources,
    state: &AerogpuD3d11State,
    vs_dxbc_hash_fnv1a64: u64,
    vs_signature: &[VsInputSignatureElement],
) -> Result<BuiltVertexState> {
    let Some(layout_handle) = state.input_layout else {
        bail!("draw without input layout");
    };
    let layout = resources
        .input_layouts
        .get_mut(&layout_handle)
        .ok_or_else(|| anyhow!("unknown input layout {layout_handle}"))?;

    let mut slot_strides = vec![0u32; MAX_INPUT_SLOTS as usize];
    for (slot, vb) in state
        .vertex_buffers
        .iter()
        .enumerate()
        .take(slot_strides.len())
    {
        if let Some(vb) = vb {
            slot_strides[slot] = vb.stride_bytes;
        }
    }

    let cache_key = hash_input_layout_mapping_key(vs_dxbc_hash_fnv1a64, &slot_strides);
    if let Some(cached) = layout.mapping_cache.get(&cache_key) {
        return Ok(cached.clone());
    }

    let fallback_signature;
    let sig = if vs_signature.is_empty() {
        fallback_signature = build_fallback_vs_signature(&layout.layout);
        fallback_signature.as_slice()
    } else {
        vs_signature
    };

    let mapped = {
        let binding = InputLayoutBinding::new(&layout.layout, &slot_strides);
        map_layout_to_shader_locations_compact(&binding, sig)
            .map_err(|e| anyhow!("input layout mapping failed: {e}"))?
    };

    let mut keys: Vec<aero_gpu::pipeline_key::VertexBufferLayoutKey> =
        Vec::with_capacity(mapped.buffers.len());
    for vb in &mapped.buffers {
        let w = vb.as_wgpu();
        keys.push((&w).into());
    }

    let mut wgpu_slot_to_d3d_slot = vec![0u32; mapped.buffers.len()];
    for (d3d_slot, wgpu_slot) in &mapped.d3d_slot_to_wgpu_slot {
        wgpu_slot_to_d3d_slot[*wgpu_slot as usize] = *d3d_slot;
    }

    let built = BuiltVertexState {
        vertex_buffers: mapped.buffers,
        vertex_buffer_keys: keys,
        wgpu_slot_to_d3d_slot,
    };
    layout.mapping_cache.insert(cache_key, built.clone());
    Ok(built)
}

struct AllocTable {
    entries: HashMap<u32, AerogpuAllocEntry>,
}

impl AllocTable {
    fn new(entries: &[AerogpuAllocEntry]) -> Result<Self> {
        let mut map = HashMap::new();
        for &e in entries {
            if e.alloc_id == 0 {
                continue;
            }
            map.insert(e.alloc_id, e);
        }
        Ok(Self { entries: map })
    }

    fn entry(&self, alloc_id: u32) -> Result<&AerogpuAllocEntry> {
        self.entries
            .get(&alloc_id)
            .ok_or_else(|| anyhow!("unknown alloc_id {alloc_id}"))
    }

    fn gpa(&self, alloc_id: u32) -> Result<u64> {
        self.entry(alloc_id).map(|e| e.gpa)
    }

    fn validate_range(&self, alloc_id: u32, offset: u64, size: u64) -> Result<()> {
        if alloc_id == 0 {
            return Ok(());
        }
        let entry = self.entry(alloc_id)?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("alloc range overflow"))?;
        if end > entry.size_bytes {
            bail!(
                "alloc {} out of range: offset=0x{:x} size=0x{:x} alloc_size=0x{:x}",
                alloc_id,
                offset,
                size,
                entry.size_bytes
            );
        }
        Ok(())
    }

    fn validate_write_range(&self, alloc_id: u32, offset: u64, size: u64) -> Result<u64> {
        if alloc_id == 0 {
            bail!("alloc_id must be non-zero for writeback");
        }
        let entry = self.entry(alloc_id)?;
        if (entry.flags & AEROGPU_ALLOC_FLAG_READONLY) != 0 {
            bail!("alloc {alloc_id} is READONLY");
        }
        self.validate_range(alloc_id, offset, size)?;
        entry
            .gpa
            .checked_add(offset)
            .ok_or_else(|| anyhow!("alloc gpa overflow"))
    }
}

fn map_pipeline_cache_stage(stage: ShaderStage) -> aero_gpu::pipeline_key::ShaderStage {
    match stage {
        ShaderStage::Vertex => aero_gpu::pipeline_key::ShaderStage::Vertex,
        ShaderStage::Pixel => aero_gpu::pipeline_key::ShaderStage::Fragment,
        ShaderStage::Compute => aero_gpu::pipeline_key::ShaderStage::Compute,
    }
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
        });
    }

    out
}

const FNV1A64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV1A64_PRIME: u64 = 0x100000001b3;

fn hash_input_layout_mapping_key(vs_dxbc_hash_fnv1a64: u64, slot_strides: &[u32]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, &vs_dxbc_hash_fnv1a64.to_le_bytes());
    fnv1a64_update(&mut hash, &(slot_strides.len() as u32).to_le_bytes());
    for &stride in slot_strides {
        fnv1a64_update(&mut hash, &stride.to_le_bytes());
    }
    hash
}

fn fnv1a64_update(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(FNV1A64_PRIME);
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, bytes);
    hash
}

fn map_buffer_usage_flags(flags: u32) -> wgpu::BufferUsages {
    let mut usage = wgpu::BufferUsages::COPY_DST;
    if flags & AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER != 0 {
        usage |= wgpu::BufferUsages::VERTEX;
    }
    if flags & AEROGPU_RESOURCE_USAGE_INDEX_BUFFER != 0 {
        usage |= wgpu::BufferUsages::INDEX;
    }
    if flags & AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER != 0 {
        usage |= wgpu::BufferUsages::UNIFORM;
    }
    // Allow readback for tests / future host interop.
    usage |= wgpu::BufferUsages::COPY_SRC;
    usage
}

fn map_texture_usage_flags(flags: u32) -> wgpu::TextureUsages {
    let mut usage = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;
    if flags & AEROGPU_RESOURCE_USAGE_TEXTURE != 0 {
        usage |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    if flags & (AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL) != 0 {
        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    if flags & AEROGPU_RESOURCE_USAGE_SCANOUT != 0 {
        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    usage
}

fn map_aerogpu_texture_format(format_u32: u32) -> Result<wgpu::TextureFormat> {
    // `enum aerogpu_format` from `aerogpu_pci.h`.
    Ok(match format_u32 {
        1 | 2 => wgpu::TextureFormat::Bgra8Unorm, // B8G8R8A8/B8G8R8X8
        3 | 4 => wgpu::TextureFormat::Rgba8Unorm, // R8G8B8A8/R8G8B8X8
        32 => wgpu::TextureFormat::Depth24PlusStencil8,
        33 => wgpu::TextureFormat::Depth32Float,
        other => bail!("unsupported aerogpu texture format {other}"),
    })
}

fn bytes_per_texel(format: wgpu::TextureFormat) -> Result<u32> {
    Ok(match format {
        wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Bgra8UnormSrgb
        | wgpu::TextureFormat::Depth24PlusStencil8
        | wgpu::TextureFormat::Depth32Float => 4,
        other => bail!("unsupported bytes_per_texel format {other:?}"),
    })
}

fn write_texture_linear(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    desc: Texture2dDesc,
    src_bytes_per_row: u32,
    bytes: &[u8],
) -> Result<()> {
    let bpt = bytes_per_texel(desc.format)?;
    let unpadded_bpr = desc
        .width
        .checked_mul(bpt)
        .ok_or_else(|| anyhow!("write_texture: bytes_per_row overflow"))?;
    if src_bytes_per_row < unpadded_bpr {
        bail!("write_texture: src_bytes_per_row too small");
    }
    let required = (src_bytes_per_row as usize).saturating_mul(desc.height as usize);
    if bytes.len() < required {
        bail!(
            "write_texture: source too small: need {} bytes, got {}",
            required,
            bytes.len()
        );
    }

    // wgpu requires bytes_per_row alignment for multi-row writes. Repack when needed.
    let aligned = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    if desc.height > 1 && src_bytes_per_row % aligned != 0 {
        let padded_bpr = ((unpadded_bpr + aligned - 1) / aligned) * aligned;
        let mut repacked = vec![0u8; (padded_bpr as usize) * (desc.height as usize)];
        for row in 0..desc.height as usize {
            let src_start = row * src_bytes_per_row as usize;
            let dst_start = row * padded_bpr as usize;
            repacked[dst_start..dst_start + unpadded_bpr as usize]
                .copy_from_slice(&bytes[src_start..src_start + unpadded_bpr as usize]);
        }
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &repacked,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(desc.height),
            },
            wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
        );
    } else {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(src_bytes_per_row),
                rows_per_image: Some(desc.height),
            },
            wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
        );
    }

    Ok(())
}

fn try_translate_sm4_signature_driven(
    dxbc: &DxbcFile<'_>,
    program: &Sm4Program,
    signatures: &crate::ShaderSignatures,
) -> Result<ShaderTranslation> {
    let module = program.decode().context("decode SM4/5 token stream")?;
    translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")
}

fn read_u32_le(buf: &[u8], offset: usize) -> Result<u32> {
    let bytes = buf
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow!("truncated u32"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_i32_le(buf: &[u8], offset: usize) -> Result<i32> {
    Ok(read_u32_le(buf, offset)? as i32)
}

fn read_u64_le(buf: &[u8], offset: usize) -> Result<u64> {
    let bytes = buf
        .get(offset..offset + 8)
        .ok_or_else(|| anyhow!("truncated u64"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_packed_unaligned<T: Copy>(bytes: &[u8]) -> Result<T> {
    let size = std::mem::size_of::<T>();
    if bytes.len() < size {
        bail!(
            "truncated packet: expected {size} bytes, got {}",
            bytes.len()
        );
    }

    // SAFETY: Bounds checked above and `read_unaligned` avoids alignment requirements.
    Ok(unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

fn texture_format_has_depth(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Depth16Unorm
            | wgpu::TextureFormat::Depth24Plus
            | wgpu::TextureFormat::Depth24PlusStencil8
            | wgpu::TextureFormat::Depth32Float
            | wgpu::TextureFormat::Depth32FloatStencil8
    )
}

fn texture_format_has_stencil(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Depth24PlusStencil8 | wgpu::TextureFormat::Depth32FloatStencil8
    )
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

fn align_copy_buffer_size(size: u64) -> Result<u64> {
    let mask = wgpu::COPY_BUFFER_ALIGNMENT - 1;
    size.checked_add(mask)
        .map(|v| v & !mask)
        .ok_or_else(|| anyhow!("buffer size overflows u64"))
}
fn map_color_write_mask(mask: u8) -> wgpu::ColorWrites {
    let mut out = wgpu::ColorWrites::empty();
    if mask & 0x1 != 0 {
        out |= wgpu::ColorWrites::RED;
    }
    if mask & 0x2 != 0 {
        out |= wgpu::ColorWrites::GREEN;
    }
    if mask & 0x4 != 0 {
        out |= wgpu::ColorWrites::BLUE;
    }
    if mask & 0x8 != 0 {
        out |= wgpu::ColorWrites::ALPHA;
    }
    out
}

fn map_blend_factor(v: u32) -> Option<wgpu::BlendFactor> {
    Some(match v {
        0 => wgpu::BlendFactor::Zero,
        1 => wgpu::BlendFactor::One,
        2 => wgpu::BlendFactor::SrcAlpha,
        3 => wgpu::BlendFactor::OneMinusSrcAlpha,
        4 => wgpu::BlendFactor::DstAlpha,
        5 => wgpu::BlendFactor::OneMinusDstAlpha,
        6 => wgpu::BlendFactor::Constant,
        7 => wgpu::BlendFactor::OneMinusConstant,
        _ => return None,
    })
}

fn map_compare_func(v: u32) -> Option<wgpu::CompareFunction> {
    Some(match v {
        0 => wgpu::CompareFunction::Never,
        1 => wgpu::CompareFunction::Less,
        2 => wgpu::CompareFunction::Equal,
        3 => wgpu::CompareFunction::LessEqual,
        4 => wgpu::CompareFunction::Greater,
        5 => wgpu::CompareFunction::NotEqual,
        6 => wgpu::CompareFunction::GreaterEqual,
        7 => wgpu::CompareFunction::Always,
        _ => return None,
    })
}

fn map_blend_op(v: u32) -> Option<wgpu::BlendOperation> {
    Some(match v {
        0 => wgpu::BlendOperation::Add,
        1 => wgpu::BlendOperation::Subtract,
        2 => wgpu::BlendOperation::ReverseSubtract,
        3 => wgpu::BlendOperation::Min,
        4 => wgpu::BlendOperation::Max,
        _ => return None,
    })
}

fn map_sampler_filter(v: u32) -> Option<wgpu::FilterMode> {
    Some(match v {
        x if x == AerogpuSamplerFilter::Nearest as u32 => wgpu::FilterMode::Nearest,
        x if x == AerogpuSamplerFilter::Linear as u32 => wgpu::FilterMode::Linear,
        _ => return None,
    })
}

fn map_sampler_address_mode(v: u32) -> Option<wgpu::AddressMode> {
    Some(match v {
        x if x == AerogpuSamplerAddressMode::ClampToEdge as u32 => wgpu::AddressMode::ClampToEdge,
        x if x == AerogpuSamplerAddressMode::Repeat as u32 => wgpu::AddressMode::Repeat,
        x if x == AerogpuSamplerAddressMode::MirrorRepeat as u32 => wgpu::AddressMode::MirrorRepeat,
        _ => return None,
    })
}

fn anyhow_guest_mem(err: GuestMemoryError) -> anyhow::Error {
    anyhow!("{err}")
}
