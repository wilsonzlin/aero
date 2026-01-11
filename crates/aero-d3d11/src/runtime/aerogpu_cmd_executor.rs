use std::collections::HashMap;
use std::ops::Range;

use aero_gpu::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use aero_gpu::pipeline_key::{ColorTargetKey, PipelineLayoutKey, RenderPipelineKey, ShaderHash};
use aero_gpu::GpuCapabilities;
use aero_gpu::{GuestMemory, GuestMemoryError};
use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, decode_cmd_stream_header_le, AerogpuCmdOpcode, AerogpuCmdStreamHeader,
    AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_CLEAR_STENCIL,
    AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_SCANOUT, AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use anyhow::{anyhow, bail, Context, Result};

use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    VertexBufferLayoutOwned, VsInputSignatureElement, MAX_INPUT_SLOTS,
};
use crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap;
use crate::{parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, Sm4Program};

const DEFAULT_MAX_VERTEX_SLOTS: usize = MAX_INPUT_SLOTS as usize;

// Opcode constants from `aerogpu_cmd.h` (via the canonical `aero-protocol` enum).
const OPCODE_NOP: u32 = AerogpuCmdOpcode::Nop as u32;
const OPCODE_DEBUG_MARKER: u32 = AerogpuCmdOpcode::DebugMarker as u32;

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_DESTROY_RESOURCE: u32 = AerogpuCmdOpcode::DestroyResource as u32;
const OPCODE_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
const OPCODE_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;

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
    backing: Option<ResourceBacking>,
    dirty: Option<Range<u64>>,
}

impl BufferResource {
    fn mark_dirty(&mut self, range: Range<u64>) {
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
    vs_input_signature: Vec<VsInputSignatureElement>,
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
}

#[derive(Debug)]
struct AerogpuD3d11Resources {
    buffers: HashMap<u32, BufferResource>,
    textures: HashMap<u32, Texture2dResource>,
    shaders: HashMap<u32, ShaderResource>,
    input_layouts: HashMap<u32, InputLayoutResource>,
}

impl Default for AerogpuD3d11Resources {
    fn default() -> Self {
        Self {
            buffers: HashMap::new(),
            textures: HashMap::new(),
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

    // A small subset of pipeline state. Unsupported values are tolerated and
    // mapped onto sensible defaults.
    blend: Option<wgpu::BlendState>,
    color_write_mask: wgpu::ColorWrites,
    cull_mode: Option<wgpu::Face>,
    front_face: wgpu::FrontFace,
    scissor_enable: bool,
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
            blend: None,
            color_write_mask: wgpu::ColorWrites::ALL,
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        }
    }
}

pub struct AerogpuD3d11Executor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    resources: AerogpuD3d11Resources,
    state: AerogpuD3d11State,

    pipeline_layout_empty: wgpu::PipelineLayout,
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

        let pipeline_layout_empty =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("aerogpu empty pipeline layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        let caps = GpuCapabilities::from_device(&device);
        let pipeline_cache = PipelineCache::new(PipelineCacheConfig::default(), caps);

        Ok(Self {
            device,
            queue,
            resources: AerogpuD3d11Resources::default(),
            state: AerogpuD3d11State::default(),
            pipeline_layout_empty,
            pipeline_cache,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn poll_wait(&self) {
        self.device.poll(wgpu::Maintain::Wait);
    }

    pub async fn read_texture_rgba8(&self, texture_id: u32) -> Result<Vec<u8>> {
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;

        if texture.desc.format != wgpu::TextureFormat::Rgba8Unorm {
            bail!("read_texture_rgba8 only supports Rgba8Unorm for now");
        }

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
        Ok(out)
    }

    pub fn execute_cmd_stream(
        &mut self,
        stream_bytes: &[u8],
        allocs: Option<&[AerogpuAllocEntry]>,
        guest_mem: &dyn GuestMemory,
    ) -> Result<ExecuteReport> {
        let hdr = decode_cmd_stream_header_le(stream_bytes)
            .map_err(|e| anyhow!("aerogpu_cmd: invalid stream header: {e:?}"))?;
        let stream_size = hdr.size_bytes as usize;
        if stream_bytes.len() < stream_size {
            bail!(
                "aerogpu_cmd: truncated stream: header size_bytes={}, buf_len={}",
                stream_size,
                stream_bytes.len()
            );
        }

        let alloc_map = AllocTable::new(allocs.unwrap_or(&[]))?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu_cmd encoder"),
            });

        let mut report = ExecuteReport::default();

        let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
        while cursor < stream_size {
            let cmd_hdr = decode_cmd_hdr_le(&stream_bytes[cursor..stream_size])
                .map_err(|e| anyhow!("aerogpu_cmd: invalid cmd header @0x{cursor:x}: {e:?}"))?;
            let cmd_size = cmd_hdr.size_bytes as usize;
            if cursor + cmd_size > stream_size {
                bail!(
                    "aerogpu_cmd: cmd overruns stream: cursor=0x{cursor:x} cmd_size=0x{cmd_size:x} stream_size=0x{stream_size:x}"
                );
            }

            let cmd_bytes = &stream_bytes[cursor..cursor + cmd_size];
            let opcode = cmd_hdr.opcode;

            // Commands that need a render-pass boundary are handled by ending any
            // in-flight pass before processing the opcode.
            match opcode {
                OPCODE_DRAW | OPCODE_DRAW_INDEXED => {
                    self.exec_render_pass_load(
                        &mut encoder,
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
            self.exec_non_draw_command(
                &mut encoder,
                opcode,
                cmd_bytes,
                &alloc_map,
                guest_mem,
                &mut report,
            )?;

            report.commands = report.commands.saturating_add(1);
            cursor += cmd_size;
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
            OPCODE_CLEAR => self.exec_clear(encoder, cmd_bytes, allocs, guest_mem),
            OPCODE_PRESENT => self.exec_present(encoder, cmd_bytes, report),
            OPCODE_PRESENT_EX => self.exec_present_ex(encoder, cmd_bytes, report),
            OPCODE_FLUSH => self.exec_flush(encoder),
            // Known-but-ignored state that should not crash bring-up.
            OPCODE_SET_BLEND_STATE => self.exec_set_blend_state(cmd_bytes),
            OPCODE_SET_DEPTH_STENCIL_STATE => Ok(()),
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
    fn exec_render_pass_load(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cursor: &mut usize,
        stream_bytes: &[u8],
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
        for handle in render_targets {
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

        let state = &self.state;
        let resources = &self.resources;

        let (_pipeline_key, pipeline, wgpu_slot_to_d3d_slot) = get_or_create_render_pipeline_for_state(
            &self.device,
            &mut self.pipeline_cache,
            &self.pipeline_layout_empty,
            resources,
            state,
        )?;

        let mut color_attachments: Vec<Option<wgpu::RenderPassColorAttachment<'_>>> =
            Vec::with_capacity(state.render_targets.len());
        for &tex_id in &state.render_targets {
            let tex = resources
                .textures
                .get(&tex_id)
                .ok_or_else(|| anyhow!("unknown render target texture {tex_id}"))?;
            color_attachments.push(Some(wgpu::RenderPassColorAttachment {
                view: &tex.view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            }));
        }

        let depth_stencil_attachment = state.depth_stencil.and_then(|ds_id| {
            resources
                .textures
                .get(&ds_id)
                .map(|tex| wgpu::RenderPassDepthStencilAttachment {
                    view: &tex.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                })
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu_cmd render pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // Apply dynamic state once at pass start.
        if let Some(vp) = state.viewport {
            pass.set_viewport(vp.x, vp.y, vp.width, vp.height, vp.min_depth, vp.max_depth);
        }
        if state.scissor_enable {
            if let Some(sc) = state.scissor {
                if sc.width > 0 && sc.height > 0 {
                    pass.set_scissor_rect(sc.x, sc.y, sc.width, sc.height);
                }
            }
        }

        pass.set_pipeline(pipeline);

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
            if *cursor >= stream_size {
                break;
            }

            let cmd_hdr = decode_cmd_hdr_le(&stream_bytes[*cursor..stream_size])
                .map_err(|e| anyhow!("aerogpu_cmd: invalid cmd header @0x{:x}: {e:?}", *cursor))?;
            let cmd_size = cmd_hdr.size_bytes as usize;
            let opcode = cmd_hdr.opcode;

            if *cursor + cmd_size > stream_size {
                bail!(
                    "aerogpu_cmd: cmd overruns stream: cursor=0x{:x} cmd_size=0x{:x} stream_size=0x{:x}",
                    *cursor,
                    cmd_size,
                    stream_size
                );
            }
            let cmd_bytes = &stream_bytes[*cursor..*cursor + cmd_size];

            match opcode {
                OPCODE_DRAW => exec_draw(&mut pass, cmd_bytes)?,
                OPCODE_DRAW_INDEXED => {
                    if state.index_buffer.is_none() {
                        bail!("DRAW_INDEXED without index buffer");
                    }
                    exec_draw_indexed(&mut pass, cmd_bytes)?;
                }
                OPCODE_NOP | OPCODE_DEBUG_MARKER => {}
                _ => break, // leave the opcode for the outer loop
            }

            report.commands = report.commands.saturating_add(1);
            *cursor += cmd_size;
        }

        drop(pass);
        Ok(())
    }

    fn build_render_pass_attachments(
        &self,
        color_load: wgpu::LoadOp<wgpu::Color>,
    ) -> Result<(
        Vec<Option<wgpu::RenderPassColorAttachment<'_>>>,
        Option<wgpu::RenderPassDepthStencilAttachment<'_>>,
    )> {
        let mut color_attachments = Vec::with_capacity(self.state.render_targets.len());
        for &tex_id in &self.state.render_targets {
            let tex = self
                .resources
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

        let depth_stencil_attachment = self.state.depth_stencil.and_then(|ds_id| {
            self.resources
                .textures
                .get(&ds_id)
                .map(|tex| wgpu::RenderPassDepthStencilAttachment {
                    view: &tex.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                })
        });

        Ok((color_attachments, depth_stencil_attachment))
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

        let usage = map_buffer_usage_flags(usage_flags);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu buffer"),
            size: size_bytes,
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
            // Only validate the allocation table range for mip0 layer0 for now.
            let bytes_per_row = if row_pitch_bytes != 0 {
                row_pitch_bytes as u64
            } else {
                (width as u64) * (bytes_per_texel(format)? as u64)
            };
            let total_size = bytes_per_row.saturating_mul(height as u64);
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
                    format,
                },
                backing,
                row_pitch_bytes,
                dirty: backing.is_some(),
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
        // struct aerogpu_cmd_upload_resource (32 bytes) + data.
        if cmd_bytes.len() < 32 {
            bail!(
                "UPLOAD_RESOURCE: expected at least 32 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        let offset = read_u64_le(cmd_bytes, 16)?;
        let size = read_u64_le(cmd_bytes, 24)?;
        let size_usize: usize = size
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: size_bytes out of range"))?;
        let data_len = align4(size_usize);
        if cmd_bytes.len() != 32 + data_len {
            bail!(
                "UPLOAD_RESOURCE: size mismatch: cmd_bytes={}, expected={}",
                cmd_bytes.len(),
                32 + data_len
            );
        }
        let data = &cmd_bytes[32..32 + size_usize];

        if let Some(buf) = self.resources.buffers.get(&handle) {
            if offset.saturating_add(size) > buf.size {
                bail!("UPLOAD_RESOURCE: buffer upload out of bounds");
            }
            self.queue.write_buffer(&buf.buffer, offset, data);
            if let Some(buf_mut) = self.resources.buffers.get_mut(&handle) {
                // Uploaded data is now current on the GPU; clear dirty ranges.
                if let Some(dirty) = buf_mut.dirty.take() {
                    // If the dirty range extends outside the uploaded region, keep it.
                    let uploaded = offset..offset + size;
                    if dirty.start < uploaded.start || dirty.end > uploaded.end {
                        buf_mut.dirty = Some(dirty);
                    }
                }
            }
            return Ok(());
        }

        if let Some(tex) = self.resources.textures.get(&handle) {
            // Minimal implementation: only supports full-texture uploads for RGBA8/BGRA8.
            let bytes_per_row = if tex.row_pitch_bytes != 0 {
                tex.row_pitch_bytes
            } else {
                tex.desc
                    .width
                    .checked_mul(bytes_per_texel(tex.desc.format)?)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: bytes_per_row overflow"))?
            };
            let expected = (bytes_per_row as u64).saturating_mul(tex.desc.height as u64);
            if offset != 0 || size != expected {
                bail!("UPLOAD_RESOURCE: only full-texture uploads are supported for now");
            }
            write_texture_linear(&self.queue, &tex.texture, tex.desc, bytes_per_row, data)?;
            if let Some(tex_mut) = self.resources.textures.get_mut(&handle) {
                tex_mut.dirty = false;
            }
            return Ok(());
        }

        Ok(())
    }

    fn exec_create_shader_dxbc(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_create_shader_dxbc (24 bytes) + dxbc bytes.
        if cmd_bytes.len() < 24 {
            bail!(
                "CREATE_SHADER_DXBC: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let shader_handle = read_u32_le(cmd_bytes, 8)?;
        let stage_u32 = read_u32_le(cmd_bytes, 12)?;
        let dxbc_size = read_u32_le(cmd_bytes, 16)? as usize;
        let expected = 24 + align4(dxbc_size);
        if cmd_bytes.len() != expected {
            bail!(
                "CREATE_SHADER_DXBC: size mismatch: cmd_bytes={}, expected={}",
                cmd_bytes.len(),
                expected
            );
        }
        let dxbc = &cmd_bytes[24..24 + dxbc_size];

        let stage = match stage_u32 {
            0 => ShaderStage::Vertex,
            1 => ShaderStage::Pixel,
            2 => ShaderStage::Compute,
            _ => bail!("CREATE_SHADER_DXBC: unknown shader stage {stage_u32}"),
        };

        let dxbc = DxbcFile::parse(dxbc).context("DXBC parse failed")?;
        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("DXBC decode failed")?;
        let parsed_stage = match program.stage {
            crate::ShaderStage::Vertex => ShaderStage::Vertex,
            crate::ShaderStage::Pixel => ShaderStage::Pixel,
            crate::ShaderStage::Compute => ShaderStage::Compute,
            other => bail!("CREATE_SHADER_DXBC: unsupported DXBC shader stage {other:?}"),
        };
        if parsed_stage != stage {
            bail!("CREATE_SHADER_DXBC: stage mismatch (cmd={stage:?}, dxbc={parsed_stage:?})");
        }

        let wgsl = match try_translate_sm4_signature_driven(&dxbc, &program, &signatures) {
            Ok(wgsl) => wgsl,
            Err(_) => translate_sm4_to_wgsl_bootstrap(&program)
                .context("DXBC->WGSL translation failed")?
                .wgsl,
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

        self.resources.shaders.insert(
            shader_handle,
            ShaderResource {
                stage,
                wgsl_hash: hash,
                vs_input_signature,
            },
        );
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
        // struct aerogpu_cmd_create_input_layout (20 bytes) + blob bytes.
        if cmd_bytes.len() < 20 {
            bail!(
                "CREATE_INPUT_LAYOUT: expected at least 20 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        let blob_size = read_u32_le(cmd_bytes, 12)? as usize;
        let expected = 20 + align4(blob_size);
        if cmd_bytes.len() != expected {
            bail!(
                "CREATE_INPUT_LAYOUT: size mismatch: cmd_bytes={}, expected={}",
                cmd_bytes.len(),
                expected
            );
        }
        let blob = &cmd_bytes[20..20 + blob_size];

        let layout = InputLayoutDesc::parse(blob)
            .map_err(|e| anyhow!("CREATE_INPUT_LAYOUT: failed to parse ILAY blob: {e}"))?;
        self.resources
            .input_layouts
            .insert(handle, InputLayoutResource { layout });
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
        for i in 0..color_count {
            let tex_id = read_u32_le(cmd_bytes, 16 + i * 4)?;
            if tex_id != 0 {
                colors.push(tex_id);
            }
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
        self.state.scissor = Some(Scissor {
            x: x.max(0) as u32,
            y: y.max(0) as u32,
            width: w as u32,
            height: h as u32,
        });
        Ok(())
    }

    fn exec_set_vertex_buffers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_vertex_buffers (16 bytes) + bindings.
        if cmd_bytes.len() < 16 {
            bail!(
                "SET_VERTEX_BUFFERS: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let start_slot = read_u32_le(cmd_bytes, 8)? as usize;
        let buffer_count = read_u32_le(cmd_bytes, 12)? as usize;
        let expected = 16 + buffer_count * 16;
        if cmd_bytes.len() != expected {
            bail!(
                "SET_VERTEX_BUFFERS: size mismatch: cmd_bytes={}, expected={}",
                cmd_bytes.len(),
                expected
            );
        }
        if start_slot + buffer_count > self.state.vertex_buffers.len() {
            bail!("SET_VERTEX_BUFFERS: slot range out of bounds");
        }

        for i in 0..buffer_count {
            let base = 16 + i * 16;
            let buffer = read_u32_le(cmd_bytes, base)?;
            let stride_bytes = read_u32_le(cmd_bytes, base + 4)?;
            let offset_bytes = read_u32_le(cmd_bytes, base + 8)? as u64;

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

    fn exec_set_blend_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_blend_state (28 bytes)
        if cmd_bytes.len() != 28 {
            bail!(
                "SET_BLEND_STATE: expected 28 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let enable = read_u32_le(cmd_bytes, 8)? != 0;
        let src_factor = read_u32_le(cmd_bytes, 12)?;
        let dst_factor = read_u32_le(cmd_bytes, 16)?;
        let op = read_u32_le(cmd_bytes, 20)?;
        let write_mask = cmd_bytes[24];

        self.state.color_write_mask = map_color_write_mask(write_mask);

        if !enable {
            self.state.blend = None;
            return Ok(());
        }

        let src = map_blend_factor(src_factor).unwrap_or(wgpu::BlendFactor::One);
        let dst = map_blend_factor(dst_factor).unwrap_or(wgpu::BlendFactor::Zero);
        let op = map_blend_op(op).unwrap_or(wgpu::BlendOperation::Add);

        self.state.blend = Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: src,
                dst_factor: dst,
                operation: op,
            },
            alpha: wgpu::BlendComponent {
                src_factor: src,
                dst_factor: dst,
                operation: op,
            },
        });
        Ok(())
    }

    fn exec_set_rasterizer_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_rasterizer_state (32 bytes)
        if cmd_bytes.len() != 32 {
            bail!(
                "SET_RASTERIZER_STATE: expected 32 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let cull_mode = read_u32_le(cmd_bytes, 16)?;
        let front_ccw = read_u32_le(cmd_bytes, 20)? != 0;
        let scissor_enable = read_u32_le(cmd_bytes, 24)? != 0;

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
        for handle in render_targets {
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

        let (mut color_attachments, mut depth_stencil_attachment) =
            self.build_render_pass_attachments(wgpu::LoadOp::Load)?;

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
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: None,
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
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: Some(d3d9_present_flags),
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
        let mut offset = dirty.start;
        while offset < dirty.end {
            let remaining = (dirty.end - offset) as usize;
            let n = remaining.min(CHUNK);
            let mut tmp = vec![0u8; n];
            guest_mem
                .read(gpa + (offset - dirty.start) as u64, &mut tmp)
                .map_err(|e| anyhow_guest_mem(e))?;
            self.queue.write_buffer(&buf.buffer, offset, &tmp);
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
        let total_size = (bytes_per_row as u64).saturating_mul(tex.desc.height as u64);
        allocs.validate_range(backing.alloc_id, backing.offset_bytes, total_size)?;
        let gpa = allocs.gpa(backing.alloc_id)? + backing.offset_bytes;

        let total_size_usize: usize = total_size
            .try_into()
            .map_err(|_| anyhow!("texture upload size out of range"))?;
        let mut tmp = vec![0u8; total_size_usize];
        guest_mem.read(gpa, &mut tmp).map_err(anyhow_guest_mem)?;

        write_texture_linear(&self.queue, &tex.texture, tex.desc, bytes_per_row, &tmp)?;
        tex.dirty = false;
        Ok(())
    }
}

#[derive(Debug)]
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

fn get_or_create_render_pipeline_for_state<'a>(
    device: &wgpu::Device,
    pipeline_cache: &'a mut PipelineCache,
    pipeline_layout_empty: &wgpu::PipelineLayout,
    resources: &AerogpuD3d11Resources,
    state: &AerogpuD3d11State,
) -> Result<(RenderPipelineKey, &'a wgpu::RenderPipeline, Vec<u32>)> {
    let vs_handle = state
        .vs
        .ok_or_else(|| anyhow!("render draw without bound VS"))?;
    let ps_handle = state
        .ps
        .ok_or_else(|| anyhow!("render draw without bound PS"))?;
    let vs = resources
        .shaders
        .get(&vs_handle)
        .ok_or_else(|| anyhow!("unknown VS shader {vs_handle}"))?;
    let ps = resources
        .shaders
        .get(&ps_handle)
        .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?;

    if vs.stage != ShaderStage::Vertex {
        bail!("shader {vs_handle} is not a vertex shader");
    }
    if ps.stage != ShaderStage::Pixel {
        bail!("shader {ps_handle} is not a pixel shader");
    }

    let BuiltVertexState {
        vertex_buffers,
        vertex_buffer_keys,
        wgpu_slot_to_d3d_slot,
    } = build_vertex_buffers_for_pipeline(resources, state, &vs.vs_input_signature)?;

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

    let key = RenderPipelineKey {
        vertex_shader: vs.wgsl_hash,
        fragment_shader: ps.wgsl_hash,
        color_targets,
        depth_stencil: None,
        primitive_topology: state.primitive_topology,
        cull_mode: state.cull_mode,
        front_face: state.front_face,
        scissor_enabled: state.scissor_enable,
        vertex_buffers: vertex_buffer_keys,
        sample_count: 1,
        layout: PipelineLayoutKey::empty(),
    };

    let topology = state.primitive_topology;
    let cull_mode = state.cull_mode;
    let front_face = state.front_face;

    let pipeline = pipeline_cache
        .get_or_create_render_pipeline(device, key.clone(), move |device, vs, fs| {
            let vb_layouts: Vec<wgpu::VertexBufferLayout<'_>> = vertex_buffers
                .iter()
                .map(VertexBufferLayoutOwned::as_wgpu)
                .collect();

            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu_cmd render pipeline"),
                layout: Some(pipeline_layout_empty),
                vertex: wgpu::VertexState {
                    module: vs,
                    entry_point: "main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &vb_layouts,
                },
                fragment: Some(wgpu::FragmentState {
                    module: fs,
                    entry_point: "main",
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
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            })
        })
        .map_err(|e| anyhow!("wgpu pipeline cache: {e:?}"))?;

    Ok((key, pipeline, wgpu_slot_to_d3d_slot))
}

fn build_vertex_buffers_for_pipeline(
    resources: &AerogpuD3d11Resources,
    state: &AerogpuD3d11State,
    vs_signature: &[VsInputSignatureElement],
) -> Result<BuiltVertexState> {
    let Some(layout_handle) = state.input_layout else {
        bail!("draw without input layout");
    };
    let layout = resources
        .input_layouts
        .get(&layout_handle)
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

    let fallback_signature;
    let sig = if vs_signature.is_empty() {
        fallback_signature = build_fallback_vs_signature(&layout.layout);
        fallback_signature.as_slice()
    } else {
        vs_signature
    };

    let binding = InputLayoutBinding::new(&layout.layout, &slot_strides);
    let mapped = map_layout_to_shader_locations_compact(&binding, sig)
        .map_err(|e| anyhow!("input layout mapping failed: {e}"))?;

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

    Ok(BuiltVertexState {
        vertex_buffers: mapped.buffers,
        vertex_buffer_keys: keys,
        wgpu_slot_to_d3d_slot,
    })
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

    fn gpa(&self, alloc_id: u32) -> Result<u64> {
        self.entries
            .get(&alloc_id)
            .map(|e| e.gpa)
            .ok_or_else(|| anyhow!("unknown alloc_id {alloc_id}"))
    }

    fn validate_range(&self, alloc_id: u32, offset: u64, size: u64) -> Result<()> {
        if alloc_id == 0 {
            return Ok(());
        }
        let entry = self
            .entries
            .get(&alloc_id)
            .ok_or_else(|| anyhow!("unknown alloc_id {alloc_id}"))?;
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
    Ok(isgn
        .parameters
        .iter()
        .map(|p| VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.as_bytes()),
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
) -> Result<String> {
    let module = program.decode().context("decode SM4/5 token stream")?;
    let translated = translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")?;

    // NOTE: `AerogpuD3d11Executor` does not yet build bind groups for translated resources. Only
    // accept the signature-driven path when the shader has no declared bindings.
    if !translated.reflection.bindings.is_empty() {
        bail!("shader requires resource bindings (not supported yet)");
    }

    Ok(translated.wgsl)
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

fn align4(n: usize) -> usize {
    (n + 3) & !3
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

fn anyhow_guest_mem(err: GuestMemoryError) -> anyhow::Error {
    anyhow!("{err}")
}
