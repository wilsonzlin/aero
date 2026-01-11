use std::collections::HashMap;

use aero_d3d9::shader;
use aero_d3d9::vertex::VertexDeclaration;
use thiserror::Error;

use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use crate::readback_rgba8;
use crate::texture_manager::TextureRegion;

/// Minimal executor for the D3D9 UMD-produced `aerogpu_cmd.h` command stream.
///
/// This is intentionally a bring-up implementation: it focuses on enough
/// resource/state tracking to render basic D3D9Ex/DWM scenes, starting with a
/// deterministic triangle test.
pub struct AerogpuD3d9Executor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    shader_cache: shader::ShaderCache,

    resources: HashMap<u32, Resource>,
    shaders: HashMap<u32, Shader>,
    input_layouts: HashMap<u32, InputLayout>,

    constants_buffer: wgpu::Buffer,

    dummy_texture_view: wgpu::TextureView,
    dummy_sampler: wgpu::Sampler,

    presented_scanouts: HashMap<u32, u32>,

    state: State,
    encoder: Option<wgpu::CommandEncoder>,
}

#[derive(Debug, Error)]
pub enum AerogpuD3d9Error {
    #[error("wgpu adapter not found")]
    AdapterNotFound,
    #[error("request_device failed: {0}")]
    RequestDevice(String),
    #[error("failed to parse AeroGPU command stream: {0}")]
    Parse(#[from] AeroGpuCmdStreamParseError),
    #[error("unknown resource handle {0}")]
    UnknownResource(u32),
    #[error("unknown shader handle {0}")]
    UnknownShader(u32),
    #[error("unknown input layout handle {0}")]
    UnknownInputLayout(u32),
    #[error("shader translation failed: {0}")]
    ShaderTranslation(String),
    #[error("shader handle {shader_handle} has stage {actual:?}, expected {expected:?}")]
    ShaderStageMismatch {
        shader_handle: u32,
        expected: shader::ShaderStage,
        actual: shader::ShaderStage,
    },
    #[error("invalid vertex declaration: {0}")]
    VertexDeclaration(String),
    #[error("draw called without a bound vertex and pixel shader")]
    MissingShaders,
    #[error("draw called without an input layout")]
    MissingInputLayout,
    #[error("draw called without any render target bound")]
    MissingRenderTargets,
    #[error("draw called without a bound vertex buffer for stream {stream}")]
    MissingVertexBuffer { stream: u8 },
    #[error("draw_indexed called without an index buffer")]
    MissingIndexBuffer,
    #[error("unsupported aerogpu_format {0}")]
    UnsupportedFormat(u32),
    #[error("unsupported primitive topology {0}")]
    UnsupportedTopology(u32),
    #[error("upload_resource target {0} is not an uploadable resource")]
    UploadNotSupported(u32),
    #[error("upload_resource out of bounds for resource {0}")]
    UploadOutOfBounds(u32),
    #[error("readback only supported for RGBA8/BGRA8 textures (handle {0})")]
    ReadbackUnsupported(u32),
}

#[derive(Debug)]
enum Resource {
    Buffer {
        buffer: wgpu::Buffer,
        size: u64,
    },
    Texture2d {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        row_pitch_bytes: u32,
    },
}

#[derive(Debug)]
struct Shader {
    stage: shader::ShaderStage,
    module: wgpu::ShaderModule,
    entry_point: &'static str,
    used_samplers: Vec<u16>,
}

#[derive(Debug)]
struct InputLayout {
    decl: VertexDeclaration,
}

#[derive(Debug)]
struct VertexInputs {
    streams: Vec<u8>,
    stream_to_slot: HashMap<u8, u32>,
    buffers: Vec<VertexBufferLayoutOwned>,
}

#[derive(Debug)]
struct VertexBufferLayoutOwned {
    array_stride: u64,
    step_mode: wgpu::VertexStepMode,
    attributes: Vec<wgpu::VertexAttribute>,
}

#[derive(Debug, Clone, Copy)]
struct VertexBufferBinding {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct IndexBufferBinding {
    buffer: u32,
    format: wgpu::IndexFormat,
    offset_bytes: u32,
}

#[derive(Debug, Default)]
struct State {
    vs: u32,
    ps: u32,
    input_layout: u32,

    render_targets: RenderTargetsState,
    viewport: Option<ViewportState>,
    scissor: Option<(u32, u32, u32, u32)>,

    vertex_buffers: [Option<VertexBufferBinding>; 16],
    index_buffer: Option<IndexBufferBinding>,
    topology: wgpu::PrimitiveTopology,

    blend_state: BlendState,
    depth_stencil_state: DepthStencilState,
    rasterizer_state: RasterizerState,

    textures_ps: [u32; 16],
}

#[derive(Debug, Clone, Copy)]
struct ViewportState {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    min_depth: f32,
    max_depth: f32,
}

#[derive(Debug, Default, Clone, Copy)]
struct RenderTargetsState {
    color_count: u32,
    colors: [u32; 8],
    depth_stencil: u32,
}

#[derive(Debug, Clone, Copy)]
struct BlendState {
    enable: bool,
    src_factor: u32,
    dst_factor: u32,
    blend_op: u32,
    color_write_mask: u8,
}

impl Default for BlendState {
    fn default() -> Self {
        Self {
            enable: false,
            // REPLACE
            src_factor: 1,
            dst_factor: 0,
            blend_op: 0,
            color_write_mask: 0xF,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DepthStencilState {
    depth_enable: bool,
    depth_write_enable: bool,
    depth_func: u32,
    stencil_enable: bool,
    stencil_read_mask: u8,
    stencil_write_mask: u8,
}

impl Default for DepthStencilState {
    fn default() -> Self {
        Self {
            depth_enable: false,
            depth_write_enable: false,
            depth_func: 7, // ALWAYS
            stencil_enable: false,
            stencil_read_mask: 0xFF,
            stencil_write_mask: 0xFF,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RasterizerState {
    cull_mode: u32,
    front_ccw: bool,
    scissor_enable: bool,
    depth_bias: i32,
}

impl Default for RasterizerState {
    fn default() -> Self {
        Self {
            cull_mode: 0,
            front_ccw: false,
            scissor_enable: false,
            depth_bias: 0,
        }
    }
}

impl AerogpuD3d9Executor {
    /// Create a headless executor suitable for tests.
    pub async fn new_headless() -> Result<Self, AerogpuD3d9Error> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or(AerogpuD3d9Error::AdapterNotFound)?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-gpu AerogpuD3d9Executor"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| AerogpuD3d9Error::RequestDevice(e.to_string()))?;

        Ok(Self::new(device, queue))
    }

    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let constants_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu-d3d9.constants"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Dummy bindings for unbound textures/samplers.
        let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu-d3d9.dummy_texture"),
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
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &dummy_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0xFF, 0xFF, 0xFF, 0xFF],
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
        let dummy_texture_view = dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aerogpu-d3d9.dummy_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            device,
            queue,
            shader_cache: shader::ShaderCache::default(),
            resources: HashMap::new(),
            shaders: HashMap::new(),
            input_layouts: HashMap::new(),
            constants_buffer,
            dummy_texture_view,
            dummy_sampler,
            presented_scanouts: HashMap::new(),
            state: State {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            encoder: None,
        }
    }

    pub fn reset(&mut self) {
        self.shader_cache = shader::ShaderCache::default();
        self.resources.clear();
        self.shaders.clear();
        self.input_layouts.clear();
        self.presented_scanouts.clear();
        self.state = State {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        };
        self.encoder = None;

        // Avoid leaking constants across resets; the next draw will rewrite what it needs.
        self.queue
            .write_buffer(&self.constants_buffer, 0, &[0u8; 256 * 16]);
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn poll(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);
    }

    pub fn execute_cmd_stream(&mut self, bytes: &[u8]) -> Result<(), AerogpuD3d9Error> {
        let stream = parse_cmd_stream(bytes)?;
        for cmd in stream.cmds {
            self.execute_cmd(cmd)?;
        }
        // Make sure we don't keep uploads queued indefinitely if the guest forgets to present.
        self.flush()
    }

    pub async fn read_presented_scanout_rgba8(
        &self,
        scanout_id: u32,
    ) -> Result<Option<(u32, u32, Vec<u8>)>, AerogpuD3d9Error> {
        let Some(&handle) = self.presented_scanouts.get(&scanout_id) else {
            return Ok(None);
        };
        let (w, h, rgba8) = self.readback_texture_rgba8(handle).await?;
        Ok(Some((w, h, rgba8)))
    }

    pub async fn readback_texture_rgba8(
        &self,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<u8>), AerogpuD3d9Error> {
        let res = self
            .resources
            .get(&texture_handle)
            .ok_or(AerogpuD3d9Error::UnknownResource(texture_handle))?;
        let (texture, format, width, height) = match res {
            Resource::Texture2d {
                texture,
                format,
                width,
                height,
                ..
            } => (texture, *format, *width, *height),
            _ => return Err(AerogpuD3d9Error::ReadbackUnsupported(texture_handle)),
        };

        let bytes = readback_rgba8(
            &self.device,
            &self.queue,
            texture,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        let out = match format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => bytes,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
                let mut rgba = bytes;
                for px in rgba.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                rgba
            }
            _ => return Err(AerogpuD3d9Error::ReadbackUnsupported(texture_handle)),
        };

        Ok((width, height, out))
    }

    fn execute_cmd(&mut self, cmd: AeroGpuCmd<'_>) -> Result<(), AerogpuD3d9Error> {
        match cmd {
            AeroGpuCmd::Nop | AeroGpuCmd::DebugMarker { .. } | AeroGpuCmd::Unknown { .. } => Ok(()),
            AeroGpuCmd::CreateBuffer {
                buffer_handle,
                size_bytes,
                ..
            } => {
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aerogpu-d3d9.buffer"),
                    size: size_bytes,
                    usage: wgpu::BufferUsages::COPY_DST
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::VERTEX
                        | wgpu::BufferUsages::INDEX,
                    mapped_at_creation: false,
                });
                self.resources.insert(
                    buffer_handle,
                    Resource::Buffer {
                        buffer,
                        size: size_bytes,
                    },
                );
                Ok(())
            }
            AeroGpuCmd::CreateTexture2d {
                texture_handle,
                format,
                width,
                height,
                mip_levels,
                array_layers,
                row_pitch_bytes,
                ..
            } => {
                let format = map_aerogpu_format(format)?;
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("aerogpu-d3d9.texture2d"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: array_layers.max(1),
                    },
                    mip_level_count: mip_levels.max(1),
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.resources.insert(
                    texture_handle,
                    Resource::Texture2d {
                        texture,
                        view,
                        format,
                        width,
                        height,
                        row_pitch_bytes,
                    },
                );
                Ok(())
            }
            AeroGpuCmd::DestroyResource { resource_handle } => {
                self.resources.remove(&resource_handle);
                Ok(())
            }
            AeroGpuCmd::UploadResource {
                resource_handle,
                offset_bytes,
                size_bytes,
                data,
            } => {
                let Some(res) = self.resources.get(&resource_handle) else {
                    return Err(AerogpuD3d9Error::UnknownResource(resource_handle));
                };
                match res {
                    Resource::Buffer { buffer, size } => {
                        let end = offset_bytes.saturating_add(size_bytes);
                        if end > *size {
                            return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                        }
                        self.queue.write_buffer(buffer, offset_bytes, data);
                        Ok(())
                    }
                    Resource::Texture2d {
                        texture,
                        format,
                        width,
                        height,
                        row_pitch_bytes,
                        ..
                    } => {
                        if offset_bytes != 0 {
                            return Err(AerogpuD3d9Error::UploadNotSupported(resource_handle));
                        }

                        let bpp = bytes_per_pixel(*format);
                        let expected_row_pitch = width.saturating_mul(bpp);
                        let src_pitch = if *row_pitch_bytes != 0 {
                            (*row_pitch_bytes).max(expected_row_pitch)
                        } else {
                            expected_row_pitch
                        };
                        let expected_len = src_pitch as usize * *height as usize;
                        if data.len() < expected_len {
                            return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                        }

                        let padded_bpr = align_to(src_pitch, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
                        let bytes = if padded_bpr != src_pitch {
                            let mut staging = vec![0u8; padded_bpr as usize * *height as usize];
                            for row in 0..*height as usize {
                                let src_start = row * src_pitch as usize;
                                let dst_start = row * padded_bpr as usize;
                                staging[dst_start..dst_start + src_pitch as usize].copy_from_slice(
                                    &data[src_start..src_start + src_pitch as usize],
                                );
                            }
                            staging
                        } else {
                            data.to_vec()
                        };

                        self.queue.write_texture(
                            wgpu::ImageCopyTexture {
                                texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            &bytes,
                            wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(padded_bpr),
                                rows_per_image: Some(*height),
                            },
                            wgpu::Extent3d {
                                width: *width,
                                height: *height,
                                depth_or_array_layers: 1,
                            },
                        );
                        Ok(())
                    }
                }
            }
            AeroGpuCmd::CreateShaderDxbc {
                shader_handle,
                stage,
                dxbc_bytes,
                ..
            } => {
                let cached = self
                    .shader_cache
                    .get_or_translate(dxbc_bytes)
                    .map_err(|e| AerogpuD3d9Error::ShaderTranslation(e.to_string()))?;
                let bytecode_stage = cached.ir.version.stage;
                let expected_stage = match stage {
                    0 => Some(shader::ShaderStage::Vertex),
                    1 => Some(shader::ShaderStage::Pixel),
                    _ => None,
                };
                if let Some(expected_stage) = expected_stage {
                    if expected_stage != bytecode_stage {
                        return Err(AerogpuD3d9Error::ShaderStageMismatch {
                            shader_handle,
                            expected: expected_stage,
                            actual: bytecode_stage,
                        });
                    }
                }
                let module = self
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("aerogpu-d3d9.shader"),
                        source: wgpu::ShaderSource::Wgsl(cached.wgsl.wgsl.clone().into()),
                    });
                let used_samplers = cached.ir.used_samplers.iter().copied().collect();
                self.shaders.insert(
                    shader_handle,
                    Shader {
                        stage: bytecode_stage,
                        module,
                        entry_point: cached.wgsl.entry_point,
                        used_samplers,
                    },
                );
                Ok(())
            }
            AeroGpuCmd::DestroyShader { shader_handle } => {
                self.shaders.remove(&shader_handle);
                Ok(())
            }
            AeroGpuCmd::BindShaders { vs, ps, .. } => {
                self.state.vs = vs;
                self.state.ps = ps;
                Ok(())
            }
            AeroGpuCmd::SetShaderConstantsF {
                start_register,
                data,
                ..
            } => {
                let offset = start_register as u64 * 16;
                self.queue
                    .write_buffer(&self.constants_buffer, offset, data);
                Ok(())
            }
            AeroGpuCmd::CreateInputLayout {
                input_layout_handle,
                blob_bytes,
                ..
            } => {
                let decl = VertexDeclaration::from_d3d_bytes(blob_bytes)
                    .map_err(|e| AerogpuD3d9Error::VertexDeclaration(e.to_string()))?;
                self.input_layouts
                    .insert(input_layout_handle, InputLayout { decl });
                Ok(())
            }
            AeroGpuCmd::DestroyInputLayout {
                input_layout_handle,
            } => {
                self.input_layouts.remove(&input_layout_handle);
                Ok(())
            }
            AeroGpuCmd::SetInputLayout {
                input_layout_handle,
            } => {
                self.state.input_layout = input_layout_handle;
                Ok(())
            }
            AeroGpuCmd::SetBlendState { state } => {
                self.state.blend_state = BlendState {
                    enable: state.enable != 0,
                    src_factor: state.src_factor,
                    dst_factor: state.dst_factor,
                    blend_op: state.blend_op,
                    color_write_mask: state.color_write_mask,
                };
                Ok(())
            }
            AeroGpuCmd::SetDepthStencilState { state } => {
                self.state.depth_stencil_state = DepthStencilState {
                    depth_enable: state.depth_enable != 0,
                    depth_write_enable: state.depth_write_enable != 0,
                    depth_func: state.depth_func,
                    stencil_enable: state.stencil_enable != 0,
                    stencil_read_mask: state.stencil_read_mask,
                    stencil_write_mask: state.stencil_write_mask,
                };
                Ok(())
            }
            AeroGpuCmd::SetRasterizerState { state } => {
                self.state.rasterizer_state = RasterizerState {
                    cull_mode: state.cull_mode,
                    front_ccw: state.front_ccw != 0,
                    scissor_enable: state.scissor_enable != 0,
                    depth_bias: state.depth_bias,
                };
                Ok(())
            }
            AeroGpuCmd::SetRenderTargets {
                color_count,
                depth_stencil,
                colors,
            } => {
                self.state.render_targets = RenderTargetsState {
                    color_count,
                    depth_stencil,
                    colors,
                };
                Ok(())
            }
            AeroGpuCmd::SetViewport {
                x_f32,
                y_f32,
                width_f32,
                height_f32,
                min_depth_f32,
                max_depth_f32,
            } => {
                self.state.viewport = Some(ViewportState {
                    x: f32::from_bits(x_f32),
                    y: f32::from_bits(y_f32),
                    width: f32::from_bits(width_f32),
                    height: f32::from_bits(height_f32),
                    min_depth: f32::from_bits(min_depth_f32),
                    max_depth: f32::from_bits(max_depth_f32),
                });
                Ok(())
            }
            AeroGpuCmd::SetScissor {
                x,
                y,
                width,
                height,
            } => {
                self.state.scissor = Some((
                    x.max(0) as u32,
                    y.max(0) as u32,
                    width.max(0) as u32,
                    height.max(0) as u32,
                ));
                Ok(())
            }
            AeroGpuCmd::SetVertexBuffers {
                start_slot,
                buffer_count,
                bindings_bytes,
            } => {
                let start = start_slot as usize;
                let count = buffer_count as usize;
                for i in 0..count {
                    let base = i * 16;
                    let binding = VertexBufferBinding {
                        buffer: u32::from_le_bytes(
                            bindings_bytes[base..base + 4].try_into().unwrap(),
                        ),
                        stride_bytes: u32::from_le_bytes(
                            bindings_bytes[base + 4..base + 8].try_into().unwrap(),
                        ),
                        offset_bytes: u32::from_le_bytes(
                            bindings_bytes[base + 8..base + 12].try_into().unwrap(),
                        ),
                    };
                    if start + i < self.state.vertex_buffers.len() {
                        self.state.vertex_buffers[start + i] = Some(binding);
                    }
                }
                Ok(())
            }
            AeroGpuCmd::SetIndexBuffer {
                buffer,
                format,
                offset_bytes,
            } => {
                let format = match format {
                    0 => wgpu::IndexFormat::Uint16,
                    1 => wgpu::IndexFormat::Uint32,
                    _ => wgpu::IndexFormat::Uint16,
                };
                self.state.index_buffer = Some(IndexBufferBinding {
                    buffer,
                    format,
                    offset_bytes,
                });
                Ok(())
            }
            AeroGpuCmd::SetPrimitiveTopology { topology } => {
                self.state.topology = map_topology(topology)?;
                Ok(())
            }
            AeroGpuCmd::SetTexture {
                shader_stage,
                slot,
                texture,
            } => {
                // For now, treat all sampler bindings as pixel shader (DWM path).
                if shader_stage == 1 {
                    if (slot as usize) < self.state.textures_ps.len() {
                        self.state.textures_ps[slot as usize] = texture;
                    }
                }
                Ok(())
            }
            AeroGpuCmd::SetSamplerState { .. } | AeroGpuCmd::SetRenderState { .. } => Ok(()),
            AeroGpuCmd::Clear {
                flags,
                color_rgba_f32,
                depth_f32,
                stencil,
            } => {
                self.ensure_encoder();
                let color_rgba = color_rgba_f32.map(f32::from_bits);
                let depth = f32::from_bits(depth_f32);
                let mut encoder = self.encoder.take().unwrap();
                let result = self.encode_clear(&mut encoder, flags, color_rgba, depth, stencil);
                self.encoder = Some(encoder);
                result
            }
            AeroGpuCmd::Draw {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => {
                self.ensure_encoder();
                let mut encoder = self.encoder.take().unwrap();
                let result = self.encode_draw(
                    &mut encoder,
                    DrawParams::NonIndexed {
                        vertex_count,
                        instance_count,
                        first_vertex,
                        first_instance,
                    },
                );
                self.encoder = Some(encoder);
                result
            }
            AeroGpuCmd::DrawIndexed {
                index_count,
                instance_count,
                first_index,
                base_vertex,
                first_instance,
            } => {
                self.ensure_encoder();
                let mut encoder = self.encoder.take().unwrap();
                let result = self.encode_draw(
                    &mut encoder,
                    DrawParams::Indexed {
                        index_count,
                        instance_count,
                        first_index,
                        base_vertex,
                        first_instance,
                    },
                );
                self.encoder = Some(encoder);
                result
            }
            AeroGpuCmd::Present { scanout_id, .. } => {
                self.record_present(scanout_id);
                self.flush()
            }
            AeroGpuCmd::PresentEx { scanout_id, .. } => {
                self.record_present(scanout_id);
                self.flush()
            }
            AeroGpuCmd::Flush => self.flush(),
            AeroGpuCmd::ExportSharedSurface { .. } | AeroGpuCmd::ImportSharedSurface { .. } => {
                // Sharing handled at higher layers for now.
                Ok(())
            }
            AeroGpuCmd::ResourceDirtyRange { .. } => Ok(()),
        }
    }

    fn record_present(&mut self, scanout_id: u32) {
        let rt = &self.state.render_targets;
        if rt.color_count == 0 {
            self.presented_scanouts.remove(&scanout_id);
            return;
        }
        let color0 = rt.colors[0];
        if color0 == 0 {
            self.presented_scanouts.remove(&scanout_id);
            return;
        }
        self.presented_scanouts.insert(scanout_id, color0);
    }

    fn ensure_encoder(&mut self) {
        if self.encoder.is_some() {
            return;
        }
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu-d3d9.encoder"),
                }),
        );
    }

    fn flush(&mut self) -> Result<(), AerogpuD3d9Error> {
        if let Some(encoder) = self.encoder.take() {
            self.queue.submit([encoder.finish()]);
        } else {
            // Still flush pending `queue.write_texture` work (wgpu requires a submit boundary).
            self.queue.submit([]);
        }
        Ok(())
    }

    fn encode_clear(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        flags: u32,
        color_rgba: [f32; 4],
        depth: f32,
        stencil: u32,
    ) -> Result<(), AerogpuD3d9Error> {
        let (color_attachments, depth_stencil) = self.render_target_attachments()?;
        let (_, depth_format) = self.render_target_formats()?;
        let depth_has_stencil =
            matches!(depth_format, Some(wgpu::TextureFormat::Depth24PlusStencil8));

        let clear_color = wgpu::Color {
            r: color_rgba[0] as f64,
            g: color_rgba[1] as f64,
            b: color_rgba[2] as f64,
            a: color_rgba[3] as f64,
        };

        let clear_color_enabled = (flags & 0x1) != 0;
        let clear_depth_enabled = (flags & 0x2) != 0;
        let clear_stencil_enabled = (flags & 0x4) != 0;

        let mut color_attachments_out = Vec::with_capacity(color_attachments.len());
        for attachment in color_attachments {
            let Some(view) = attachment else {
                color_attachments_out.push(None);
                continue;
            };
            color_attachments_out.push(Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if clear_color_enabled {
                        wgpu::LoadOp::Clear(clear_color)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            }));
        }

        let depth_attachment = depth_stencil.map(|view| wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: Some(wgpu::Operations {
                load: if clear_depth_enabled {
                    wgpu::LoadOp::Clear(depth)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: depth_has_stencil.then(|| wgpu::Operations {
                load: if clear_stencil_enabled {
                    wgpu::LoadOp::Clear(stencil)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            }),
        });

        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu-d3d9.clear"),
            color_attachments: &color_attachments_out,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        Ok(())
    }

    fn encode_draw(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        draw: DrawParams,
    ) -> Result<(), AerogpuD3d9Error> {
        let vs_handle = self.state.vs;
        let ps_handle = self.state.ps;
        if vs_handle == 0 || ps_handle == 0 {
            return Err(AerogpuD3d9Error::MissingShaders);
        }
        let vs = self
            .shaders
            .get(&vs_handle)
            .ok_or(AerogpuD3d9Error::UnknownShader(vs_handle))?;
        let ps = self
            .shaders
            .get(&ps_handle)
            .ok_or(AerogpuD3d9Error::UnknownShader(ps_handle))?;
        if vs.stage != shader::ShaderStage::Vertex {
            return Err(AerogpuD3d9Error::ShaderStageMismatch {
                shader_handle: vs_handle,
                expected: shader::ShaderStage::Vertex,
                actual: vs.stage,
            });
        }
        if ps.stage != shader::ShaderStage::Pixel {
            return Err(AerogpuD3d9Error::ShaderStageMismatch {
                shader_handle: ps_handle,
                expected: shader::ShaderStage::Pixel,
                actual: ps.stage,
            });
        }

        let layout_handle = self.state.input_layout;
        if layout_handle == 0 {
            return Err(AerogpuD3d9Error::MissingInputLayout);
        }
        let layout = self
            .input_layouts
            .get(&layout_handle)
            .ok_or(AerogpuD3d9Error::UnknownInputLayout(layout_handle))?;

        let (color_views, depth_view) = self.render_target_attachments()?;
        let (color_formats, depth_format) = self.render_target_formats()?;
        let depth_has_stencil =
            matches!(depth_format, Some(wgpu::TextureFormat::Depth24PlusStencil8));

        // Bind group layout: binding(0)=constants, then (texture,sampler) pairs.
        // For now we only support pixel-stage samplers (D3D9Ex/DWM).
        let mut bgl_entries = Vec::new();
        bgl_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(256 * 16),
            },
            count: None,
        });

        let used_samplers: Vec<u16> = vs
            .used_samplers
            .iter()
            .copied()
            .chain(ps.used_samplers.iter().copied())
            .collect();
        let mut used_samplers = used_samplers;
        used_samplers.sort_unstable();
        used_samplers.dedup();

        // Match `aero-d3d9` shader generation: bindings are allocated sequentially for the
        // *used* sampler indices (not `1 + 2*sampler_index`).
        let mut next_binding = 1u32;
        for _sampler in &used_samplers {
            let tex_binding = next_binding;
            let samp_binding = next_binding + 1;
            next_binding += 2;
            bgl_entries.push(wgpu::BindGroupLayoutEntry {
                binding: tex_binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            });
            bgl_entries.push(wgpu::BindGroupLayoutEntry {
                binding: samp_binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
        }

        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("aerogpu-d3d9.bind_group_layout"),
                    entries: &bgl_entries,
                });

        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("aerogpu-d3d9.pipeline_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let bind_group = self.create_bind_group(&bind_group_layout, &used_samplers)?;

        let vertex_buffers = self.vertex_buffer_layouts(layout, vs)?;
        let vertex_buffers_ref = vertex_buffers
            .buffers
            .iter()
            .map(|b| wgpu::VertexBufferLayout {
                array_stride: b.array_stride,
                step_mode: b.step_mode,
                attributes: &b.attributes,
            })
            .collect::<Vec<_>>();

        let targets = color_formats
            .iter()
            .map(|fmt| {
                fmt.map(|format| wgpu::ColorTargetState {
                    format,
                    blend: map_blend_state(self.state.blend_state),
                    write_mask: map_color_write_mask(self.state.blend_state.color_write_mask),
                })
            })
            .collect::<Vec<_>>();

        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu-d3d9.pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &vs.module,
                    entry_point: vs.entry_point,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &vertex_buffers_ref,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &ps.module,
                    entry_point: ps.entry_point,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &targets,
                }),
                primitive: wgpu::PrimitiveState {
                    topology: self.state.topology,
                    strip_index_format: None,
                    front_face: if self.state.rasterizer_state.front_ccw {
                        wgpu::FrontFace::Ccw
                    } else {
                        wgpu::FrontFace::Cw
                    },
                    cull_mode: match self.state.rasterizer_state.cull_mode {
                        1 => Some(wgpu::Face::Front),
                        2 => Some(wgpu::Face::Back),
                        _ => None,
                    },
                    ..Default::default()
                },
                depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
                    format,
                    depth_write_enabled: self.state.depth_stencil_state.depth_enable
                        && self.state.depth_stencil_state.depth_write_enable,
                    depth_compare: if self.state.depth_stencil_state.depth_enable {
                        map_compare_func(self.state.depth_stencil_state.depth_func)
                    } else {
                        wgpu::CompareFunction::Always
                    },
                    stencil: if depth_has_stencil && self.state.depth_stencil_state.stencil_enable {
                        wgpu::StencilState {
                            front: wgpu::StencilFaceState::IGNORE,
                            back: wgpu::StencilFaceState::IGNORE,
                            read_mask: self.state.depth_stencil_state.stencil_read_mask as u32,
                            write_mask: self.state.depth_stencil_state.stencil_write_mask as u32,
                        }
                    } else {
                        wgpu::StencilState {
                            front: wgpu::StencilFaceState::IGNORE,
                            back: wgpu::StencilFaceState::IGNORE,
                            read_mask: 0,
                            write_mask: 0,
                        }
                    },
                    bias: wgpu::DepthBiasState {
                        constant: self.state.rasterizer_state.depth_bias,
                        slope_scale: 0.0,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        let color_attachments = color_views
            .into_iter()
            .map(|view| {
                view.map(|view| wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect::<Vec<_>>();

        let depth_stencil_attachment =
            depth_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: depth_has_stencil.then(|| wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
            });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu-d3d9.render"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        if let Some(viewport) = self.state.viewport.as_ref() {
            pass.set_viewport(
                viewport.x,
                viewport.y,
                viewport.width,
                viewport.height,
                viewport.min_depth,
                viewport.max_depth,
            );
        }

        if self.state.rasterizer_state.scissor_enable {
            if let Some((x, y, w, h)) = self.state.scissor {
                pass.set_scissor_rect(x, y, w, h);
            }
        }

        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);

        // Bind vertex buffers: wgpu slot is derived from the vertex declaration's used streams.
        for stream in &vertex_buffers.streams {
            let Some(binding) = self
                .state
                .vertex_buffers
                .get(*stream as usize)
                .copied()
                .flatten()
            else {
                continue;
            };
            let res = self
                .resources
                .get(&binding.buffer)
                .ok_or(AerogpuD3d9Error::UnknownResource(binding.buffer))?;
            let Resource::Buffer { buffer, .. } = res else {
                return Err(AerogpuD3d9Error::UnknownResource(binding.buffer));
            };
            let wgpu_slot = *vertex_buffers
                .stream_to_slot
                .get(stream)
                .expect("stream_to_slot contains all streams");
            pass.set_vertex_buffer(wgpu_slot, buffer.slice(binding.offset_bytes as u64..));
        }

        match draw {
            DrawParams::NonIndexed {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => {
                pass.draw(
                    first_vertex..first_vertex + vertex_count,
                    first_instance..first_instance + instance_count,
                );
            }
            DrawParams::Indexed {
                index_count,
                instance_count,
                first_index,
                base_vertex,
                first_instance,
            } => {
                let index_binding = self
                    .state
                    .index_buffer
                    .ok_or(AerogpuD3d9Error::MissingIndexBuffer)?;
                let res = self
                    .resources
                    .get(&index_binding.buffer)
                    .ok_or(AerogpuD3d9Error::UnknownResource(index_binding.buffer))?;
                let Resource::Buffer { buffer, .. } = res else {
                    return Err(AerogpuD3d9Error::UnknownResource(index_binding.buffer));
                };
                pass.set_index_buffer(
                    buffer.slice(index_binding.offset_bytes as u64..),
                    index_binding.format,
                );
                pass.draw_indexed(
                    first_index..first_index + index_count,
                    base_vertex,
                    first_instance..first_instance + instance_count,
                );
            }
        }

        Ok(())
    }

    fn create_bind_group(
        &self,
        layout: &wgpu::BindGroupLayout,
        used_samplers: &[u16],
    ) -> Result<wgpu::BindGroup, AerogpuD3d9Error> {
        let mut entries: Vec<wgpu::BindGroupEntry> = Vec::new();
        entries.push(wgpu::BindGroupEntry {
            binding: 0,
            resource: self.constants_buffer.as_entire_binding(),
        });

        // Must use the same sequential binding assignment as above.
        let mut next_binding = 1u32;
        for s in used_samplers {
            let tex_binding = next_binding;
            let samp_binding = next_binding + 1;
            next_binding += 2;
            let tex_handle = self
                .state
                .textures_ps
                .get(*s as usize)
                .copied()
                .unwrap_or(0);
            let view: &wgpu::TextureView = if tex_handle == 0 {
                &self.dummy_texture_view
            } else {
                match self.resources.get(&tex_handle) {
                    Some(Resource::Texture2d { view, .. }) => view,
                    _ => &self.dummy_texture_view,
                }
            };

            entries.push(wgpu::BindGroupEntry {
                binding: tex_binding,
                resource: wgpu::BindingResource::TextureView(view),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: samp_binding,
                resource: wgpu::BindingResource::Sampler(&self.dummy_sampler),
            });
        }

        Ok(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu-d3d9.bind_group"),
            layout,
            entries: &entries,
        }))
    }

    fn vertex_buffer_layouts(
        &self,
        input_layout: &InputLayout,
        _vs: &Shader,
    ) -> Result<VertexInputs, AerogpuD3d9Error> {
        let mut streams: Vec<u8> = input_layout
            .decl
            .elements
            .iter()
            .map(|e| e.stream)
            .collect();
        streams.sort_unstable();
        streams.dedup();

        let mut stream_to_slot = HashMap::<u8, u32>::new();
        for (slot, stream) in streams.iter().copied().enumerate() {
            stream_to_slot.insert(stream, slot as u32);
        }

        let mut buffers: Vec<VertexBufferLayoutOwned> = streams
            .iter()
            .map(|stream| {
                let stride = self
                    .state
                    .vertex_buffers
                    .get(*stream as usize)
                    .and_then(|b| b.as_ref())
                    .map(|b| b.stride_bytes as u64)
                    .unwrap_or(0);
                VertexBufferLayoutOwned {
                    array_stride: stride,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: Vec::new(),
                }
            })
            .collect();

        // Bring-up behavior: map declaration elements to sequential shader locations.
        for (i, e) in input_layout.decl.elements.iter().enumerate() {
            let Some(&slot) = stream_to_slot.get(&e.stream) else {
                continue;
            };
            let fmt = map_decl_type_to_vertex_format(e.ty)?;
            buffers[slot as usize]
                .attributes
                .push(wgpu::VertexAttribute {
                    format: fmt,
                    offset: e.offset as u64,
                    shader_location: i as u32,
                });
        }

        for (i, stream) in streams.iter().copied().enumerate() {
            if buffers[i].attributes.is_empty() {
                continue;
            }
            if buffers[i].array_stride == 0 {
                return Err(AerogpuD3d9Error::MissingVertexBuffer { stream });
            }
        }

        Ok(VertexInputs {
            streams,
            stream_to_slot,
            buffers,
        })
    }

    fn render_target_attachments(
        &self,
    ) -> Result<(Vec<Option<&wgpu::TextureView>>, Option<&wgpu::TextureView>), AerogpuD3d9Error>
    {
        let rt = &self.state.render_targets;
        if rt.color_count == 0 {
            return Err(AerogpuD3d9Error::MissingRenderTargets);
        }
        let mut colors = Vec::new();
        for slot in 0..rt.color_count.min(8) as usize {
            let handle = rt.colors[slot];
            if handle == 0 {
                colors.push(None);
                continue;
            }
            let res = self
                .resources
                .get(&handle)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { view, .. } => colors.push(Some(view)),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        }

        let depth = if rt.depth_stencil == 0 {
            None
        } else {
            let handle = rt.depth_stencil;
            let res = self
                .resources
                .get(&handle)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { view, .. } => Some(view),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        };

        Ok((colors, depth))
    }

    fn render_target_formats(
        &self,
    ) -> Result<
        (
            Vec<Option<wgpu::TextureFormat>>,
            Option<wgpu::TextureFormat>,
        ),
        AerogpuD3d9Error,
    > {
        let rt = &self.state.render_targets;
        if rt.color_count == 0 {
            return Err(AerogpuD3d9Error::MissingRenderTargets);
        }
        let mut colors = Vec::new();
        for slot in 0..rt.color_count.min(8) as usize {
            let handle = rt.colors[slot];
            if handle == 0 {
                colors.push(None);
                continue;
            }
            let res = self
                .resources
                .get(&handle)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { format, .. } => colors.push(Some(*format)),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        }

        let depth = if rt.depth_stencil == 0 {
            None
        } else {
            let handle = rt.depth_stencil;
            let res = self
                .resources
                .get(&handle)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { format, .. } => Some(*format),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        };

        Ok((colors, depth))
    }
}

#[derive(Debug, Clone, Copy)]
enum DrawParams {
    NonIndexed {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    Indexed {
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    },
}

fn align_to(value: u32, alignment: u32) -> u32 {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

fn map_aerogpu_format(format: u32) -> Result<wgpu::TextureFormat, AerogpuD3d9Error> {
    Ok(match format {
        // AEROGPU_FORMAT_B8G8R8A8_UNORM
        1 | 2 => wgpu::TextureFormat::Bgra8Unorm,
        // AEROGPU_FORMAT_R8G8B8A8_UNORM
        3 | 4 => wgpu::TextureFormat::Rgba8Unorm,
        // AEROGPU_FORMAT_D24_UNORM_S8_UINT
        32 => wgpu::TextureFormat::Depth24PlusStencil8,
        // AEROGPU_FORMAT_D32_FLOAT
        33 => wgpu::TextureFormat::Depth32Float,
        other => return Err(AerogpuD3d9Error::UnsupportedFormat(other)),
    })
}

fn bytes_per_pixel(format: wgpu::TextureFormat) -> u32 {
    match format {
        wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Bgra8UnormSrgb => 4,
        wgpu::TextureFormat::Depth24PlusStencil8 => 4,
        wgpu::TextureFormat::Depth32Float => 4,
        _ => 4,
    }
}

fn map_topology(topology: u32) -> Result<wgpu::PrimitiveTopology, AerogpuD3d9Error> {
    Ok(match topology {
        1 => wgpu::PrimitiveTopology::PointList,
        2 => wgpu::PrimitiveTopology::LineList,
        3 => wgpu::PrimitiveTopology::LineStrip,
        4 => wgpu::PrimitiveTopology::TriangleList,
        5 => wgpu::PrimitiveTopology::TriangleStrip,
        6 => wgpu::PrimitiveTopology::TriangleList, // TriangleFan: approximated for now.
        other => return Err(AerogpuD3d9Error::UnsupportedTopology(other)),
    })
}

fn map_decl_type_to_vertex_format(
    ty: aero_d3d9::vertex::DeclType,
) -> Result<wgpu::VertexFormat, AerogpuD3d9Error> {
    use aero_d3d9::vertex::DeclType;
    Ok(match ty {
        DeclType::Float1 => wgpu::VertexFormat::Float32,
        DeclType::Float2 => wgpu::VertexFormat::Float32x2,
        DeclType::Float3 => wgpu::VertexFormat::Float32x3,
        DeclType::Float4 => wgpu::VertexFormat::Float32x4,
        DeclType::D3dColor => wgpu::VertexFormat::Unorm8x4,
        DeclType::UByte4N => wgpu::VertexFormat::Unorm8x4,
        DeclType::Unused => wgpu::VertexFormat::Float32x4,
        _ => wgpu::VertexFormat::Float32x4,
    })
}

fn map_color_write_mask(mask: u8) -> wgpu::ColorWrites {
    let mut out = wgpu::ColorWrites::empty();
    if mask & 0b0001 != 0 {
        out |= wgpu::ColorWrites::RED;
    }
    if mask & 0b0010 != 0 {
        out |= wgpu::ColorWrites::GREEN;
    }
    if mask & 0b0100 != 0 {
        out |= wgpu::ColorWrites::BLUE;
    }
    if mask & 0b1000 != 0 {
        out |= wgpu::ColorWrites::ALPHA;
    }
    out
}

fn map_blend_state(state: BlendState) -> Option<wgpu::BlendState> {
    if !state.enable {
        return None;
    }

    let component = wgpu::BlendComponent {
        src_factor: map_blend_factor(state.src_factor),
        dst_factor: map_blend_factor(state.dst_factor),
        operation: map_blend_op(state.blend_op),
    };
    Some(wgpu::BlendState {
        color: component,
        alpha: component,
    })
}

fn map_blend_factor(factor: u32) -> wgpu::BlendFactor {
    match factor {
        0 => wgpu::BlendFactor::Zero,
        1 => wgpu::BlendFactor::One,
        2 => wgpu::BlendFactor::SrcAlpha,
        3 => wgpu::BlendFactor::OneMinusSrcAlpha,
        4 => wgpu::BlendFactor::DstAlpha,
        5 => wgpu::BlendFactor::OneMinusDstAlpha,
        _ => wgpu::BlendFactor::One,
    }
}

fn map_blend_op(op: u32) -> wgpu::BlendOperation {
    match op {
        0 => wgpu::BlendOperation::Add,
        1 => wgpu::BlendOperation::Subtract,
        2 => wgpu::BlendOperation::ReverseSubtract,
        3 => wgpu::BlendOperation::Min,
        4 => wgpu::BlendOperation::Max,
        _ => wgpu::BlendOperation::Add,
    }
}

fn map_compare_func(func: u32) -> wgpu::CompareFunction {
    match func {
        0 => wgpu::CompareFunction::Never,
        1 => wgpu::CompareFunction::Less,
        2 => wgpu::CompareFunction::Equal,
        3 => wgpu::CompareFunction::LessEqual,
        4 => wgpu::CompareFunction::Greater,
        5 => wgpu::CompareFunction::NotEqual,
        6 => wgpu::CompareFunction::GreaterEqual,
        7 => wgpu::CompareFunction::Always,
        _ => wgpu::CompareFunction::Always,
    }
}
