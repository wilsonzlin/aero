use std::collections::HashMap;

use futures_intrusive::channel::shared::oneshot_channel;
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct RuntimeConfig {
    pub validation: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self { validation: false }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SwapChainDesc {
    pub width: u32,
    pub height: u32,
    pub format: ColorFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFormat {
    Rgba8Unorm,
    Rgba8UnormSrgb,
}

impl ColorFormat {
    fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            Self::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Color(ColorFormat),
    Depth24PlusStencil8,
}

impl TextureFormat {
    fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Color(format) => format.to_wgpu(),
            Self::Depth24PlusStencil8 => wgpu::TextureFormat::Depth24PlusStencil8,
        }
    }

    fn as_color(self) -> Option<ColorFormat> {
        match self {
            Self::Color(format) => Some(format),
            Self::Depth24PlusStencil8 => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RenderTarget {
    SwapChain(u32),
    Texture(u32),
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("wgpu adapter not found")]
    AdapterNotFound,
    #[error("request_device failed: {0}")]
    RequestDevice(String),
    #[error("swapchain {0} already exists")]
    SwapChainAlreadyExists(u32),
    #[error("unknown swapchain {0}")]
    UnknownSwapChain(u32),
    #[error("buffer {0} already exists")]
    BufferAlreadyExists(u32),
    #[error("unknown buffer {0}")]
    UnknownBuffer(u32),
    #[error("buffer write out of bounds (buffer size {buffer_size}, write end {write_end})")]
    BufferWriteOutOfBounds { buffer_size: u64, write_end: u64 },
    #[error("texture {0} already exists")]
    TextureAlreadyExists(u32),
    #[error("unknown texture {0}")]
    UnknownTexture(u32),
    #[error("texture update for mip {mip_level} expects {expected} bytes but got {actual}")]
    TextureUpdateSizeMismatch {
        mip_level: u32,
        expected: usize,
        actual: usize,
    },
    #[error("texture update provided dimensions {provided_width}x{provided_height} but mip {mip_level} is {expected_width}x{expected_height}")]
    TextureUpdateDimensionsMismatch {
        mip_level: u32,
        expected_width: u32,
        expected_height: u32,
        provided_width: u32,
        provided_height: u32,
    },
    #[error("texture format {0:?} cannot be used as a color render target")]
    TextureNotColorRenderable(TextureFormat),
    #[error("texture format {0:?} cannot be used as a depth-stencil render target")]
    TextureNotDepthRenderable(TextureFormat),
    #[error("texture updates are not supported for format {0:?}")]
    UnsupportedTextureUpdateFormat(TextureFormat),
    #[error("draw called without a render target")]
    MissingRenderTarget,
    #[error("draw called without both vertex and fragment shaders set")]
    MissingShaders,
    #[error("draw called without a vertex declaration")]
    MissingVertexDeclaration,
    #[error("draw called without a vertex buffer on stream 0")]
    MissingVertexBuffer,
    #[error("draw_indexed called without an index buffer")]
    MissingIndexBuffer,
    #[error("unsupported shader key {0}")]
    UnsupportedShaderKey(u32),
    #[error("unsupported constants update (stage {stage:?}, register {start_register}, vec4 count {vec4_count})")]
    UnsupportedConstantsUpdate {
        stage: ShaderStage,
        start_register: u16,
        vec4_count: u16,
    },
    #[error("texture readback only supported for RGBA8 formats, got {0:?}")]
    UnsupportedReadbackFormat(wgpu::TextureFormat),
    #[error("map_async callback dropped unexpectedly")]
    MapAsyncDropped,
    #[error("map_async failed: {0}")]
    MapAsync(String),
    #[error("wgpu validation error: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Fragment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexFormat {
    U16,
    U32,
}

impl IndexFormat {
    fn to_wgpu(self) -> wgpu::IndexFormat {
        match self {
            Self::U16 => wgpu::IndexFormat::Uint16,
            Self::U32 => wgpu::IndexFormat::Uint32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VertexFormat {
    Float32x2,
    Float32x3,
    Float32x4,
    Unorm8x4,
}

impl VertexFormat {
    fn to_wgpu(self) -> wgpu::VertexFormat {
        match self {
            Self::Float32x2 => wgpu::VertexFormat::Float32x2,
            Self::Float32x3 => wgpu::VertexFormat::Float32x3,
            Self::Float32x4 => wgpu::VertexFormat::Float32x4,
            Self::Unorm8x4 => wgpu::VertexFormat::Unorm8x4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VertexAttributeDesc {
    pub location: u32,
    pub format: VertexFormat,
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VertexDecl {
    pub stride: u64,
    pub attributes: Vec<VertexAttributeDesc>,
}

#[derive(Debug)]
struct BufferResource {
    buffer: wgpu::Buffer,
    size: u64,
}

#[derive(Debug)]
struct SwapChainResource {
    desc: SwapChainDesc,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Debug, Clone, Copy)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub mip_level_count: u32,
    pub format: TextureFormat,
    pub usage: u32,
}

#[derive(Debug)]
struct TextureResource {
    desc: TextureDesc,
    texture: wgpu::Texture,
    view_mip0: wgpu::TextureView,
}

#[derive(Debug, Clone, Copy)]
struct VertexStreamBinding {
    buffer_id: u32,
    offset: u64,
    stride: u64,
}

#[derive(Debug, Clone, Copy)]
struct IndexBinding {
    buffer_id: u32,
    offset: u64,
    format: IndexFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ShaderId(u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PipelineKey {
    vs: ShaderId,
    fs: ShaderId,
    vertex_decl: VertexDecl,
    color_format: ColorFormat,
    cull_mode: CullMode,
    has_depth: bool,
    depth_test_enable: bool,
    depth_write_enable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CullMode {
    None,
    Front,
    Back,
}

#[derive(Debug)]
struct GraphicsState {
    color_target: Option<RenderTarget>,
    depth_stencil: Option<u32>,
    vertex_shader: Option<ShaderId>,
    fragment_shader: Option<ShaderId>,
    vertex_decl: Option<VertexDecl>,
    vertex_stream0: Option<VertexStreamBinding>,
    index_buffer: Option<IndexBinding>,
    cull_mode: CullMode,
    depth_test_enable: bool,
    depth_write_enable: bool,
    encoder: Option<wgpu::CommandEncoder>,
    encoder_needs_clear: bool,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            color_target: None,
            depth_stencil: None,
            vertex_shader: None,
            fragment_shader: None,
            vertex_decl: None,
            vertex_stream0: None,
            index_buffer: None,
            cull_mode: CullMode::None,
            depth_test_enable: false,
            depth_write_enable: false,
            encoder: None,
            encoder_needs_clear: true,
        }
    }
}

pub struct D3D9Runtime {
    config: RuntimeConfig,
    device: wgpu::Device,
    queue: wgpu::Queue,

    buffers: HashMap<u32, BufferResource>,
    swapchains: HashMap<u32, SwapChainResource>,
    textures: HashMap<u32, TextureResource>,

    builtin_shader_module: Option<wgpu::ShaderModule>,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,

    constants_buffer: wgpu::Buffer,
    constants_bind_group: wgpu::BindGroup,
    pipeline_layout: wgpu::PipelineLayout,

    state: GraphicsState,
    fences: HashMap<u32, u64>,
}

impl D3D9Runtime {
    pub async fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RuntimeError::AdapterNotFound)?;

        let descriptor = wgpu::DeviceDescriptor {
            label: Some("aero-d3d9-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        };

        let (device, queue) = adapter
            .request_device(&descriptor, None)
            .await
            .map_err(|e| RuntimeError::RequestDevice(e.to_string()))?;

        let constants_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("aero-d3d9-constants-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d9-pipeline-layout"),
            bind_group_layouts: &[&constants_bind_group_layout],
            push_constant_ranges: &[],
        });

        let constants_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d9-constants-ubo"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let constants_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d9-constants-bg"),
            layout: &constants_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: constants_buffer.as_entire_binding(),
            }],
        });

        queue.write_buffer(
            &constants_buffer,
            0,
            bytemuck::bytes_of(&[1.0f32, 1.0, 1.0, 1.0]),
        );

        Ok(Self {
            config,
            device,
            queue,
            buffers: HashMap::new(),
            swapchains: HashMap::new(),
            textures: HashMap::new(),
            builtin_shader_module: None,
            pipelines: HashMap::new(),
            constants_buffer,
            constants_bind_group,
            pipeline_layout,
            state: GraphicsState::default(),
            fences: HashMap::new(),
        })
    }

    pub fn begin_validation_scope(&self) {
        if !self.config.validation {
            return;
        }
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
    }

    pub async fn end_validation_scope(&self) -> Option<String> {
        if !self.config.validation {
            return None;
        }
        self.device.pop_error_scope().await.map(|e| e.to_string())
    }

    pub fn create_swap_chain(
        &mut self,
        swapchain_id: u32,
        desc: SwapChainDesc,
    ) -> Result<(), RuntimeError> {
        if self.swapchains.contains_key(&swapchain_id) {
            return Err(RuntimeError::SwapChainAlreadyExists(swapchain_id));
        }

        let format = desc.format.to_wgpu();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d9-swapchain-texture"),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.swapchains.insert(
            swapchain_id,
            SwapChainResource {
                desc,
                texture,
                view,
            },
        );
        Ok(())
    }

    pub fn destroy_swap_chain(&mut self, swapchain_id: u32) -> Result<(), RuntimeError> {
        self.swapchains
            .remove(&swapchain_id)
            .ok_or(RuntimeError::UnknownSwapChain(swapchain_id))?;
        if matches!(self.state.color_target, Some(RenderTarget::SwapChain(id)) if id == swapchain_id)
        {
            self.state.color_target = None;
        }
        Ok(())
    }

    pub fn create_texture(
        &mut self,
        texture_id: u32,
        desc: TextureDesc,
    ) -> Result<(), RuntimeError> {
        if self.textures.contains_key(&texture_id) {
            return Err(RuntimeError::TextureAlreadyExists(texture_id));
        }

        let format = desc.format.to_wgpu();
        let usage = map_texture_usage(desc.usage)
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d9-texture"),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: desc.mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });

        let view_mip0 = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("aero-d3d9-texture-mip0"),
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(1),
            dimension: Some(wgpu::TextureViewDimension::D2),
            format: None,
            aspect: wgpu::TextureAspect::All,
        });

        self.textures.insert(
            texture_id,
            TextureResource {
                desc,
                texture,
                view_mip0,
            },
        );
        Ok(())
    }

    pub fn write_texture_full_mip(
        &mut self,
        texture_id: u32,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<(), RuntimeError> {
        let texture = self
            .textures
            .get(&texture_id)
            .ok_or(RuntimeError::UnknownTexture(texture_id))?;

        let expected_width = (texture.desc.width >> mip_level).max(1);
        let expected_height = (texture.desc.height >> mip_level).max(1);
        if width != expected_width || height != expected_height {
            return Err(RuntimeError::TextureUpdateDimensionsMismatch {
                mip_level,
                expected_width,
                expected_height,
                provided_width: width,
                provided_height: height,
            });
        }

        if texture.desc.format.as_color().is_none() {
            return Err(RuntimeError::UnsupportedTextureUpdateFormat(
                texture.desc.format,
            ));
        }

        let bytes_per_pixel = 4u32;
        let expected_size = width as usize * height as usize * bytes_per_pixel as usize;
        if data.len() != expected_size {
            return Err(RuntimeError::TextureUpdateSizeMismatch {
                mip_level,
                expected: expected_size,
                actual: data.len(),
            });
        }

        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let padded_bytes_per_row =
            align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let staging;
        let bytes = if padded_bytes_per_row == unpadded_bytes_per_row {
            data
        } else {
            staging = pad_rows(
                data,
                unpadded_bytes_per_row as usize,
                padded_bytes_per_row as usize,
                height as usize,
            );
            &staging
        };

        // Use TextureAspect::All; depth-stencil updates are currently rejected above.
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture.texture,
                mip_level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
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

    pub fn destroy_texture(&mut self, texture_id: u32) -> Result<(), RuntimeError> {
        self.textures
            .remove(&texture_id)
            .ok_or(RuntimeError::UnknownTexture(texture_id))?;

        if matches!(self.state.color_target, Some(RenderTarget::Texture(id)) if id == texture_id) {
            self.state.color_target = None;
        }
        if self.state.depth_stencil == Some(texture_id) {
            self.state.depth_stencil = None;
        }

        Ok(())
    }

    pub fn set_render_targets(
        &mut self,
        color: Option<RenderTarget>,
        depth_stencil: Option<u32>,
    ) -> Result<(), RuntimeError> {
        if let Some(target) = color {
            match target {
                RenderTarget::SwapChain(id) => {
                    if !self.swapchains.contains_key(&id) {
                        return Err(RuntimeError::UnknownSwapChain(id));
                    }
                }
                RenderTarget::Texture(id) => {
                    let tex = self
                        .textures
                        .get(&id)
                        .ok_or(RuntimeError::UnknownTexture(id))?;
                    if tex.desc.format.as_color().is_none() {
                        return Err(RuntimeError::TextureNotColorRenderable(tex.desc.format));
                    }
                }
            }
        }

        if let Some(depth_id) = depth_stencil {
            let tex = self
                .textures
                .get(&depth_id)
                .ok_or(RuntimeError::UnknownTexture(depth_id))?;
            if tex.desc.format != TextureFormat::Depth24PlusStencil8 {
                return Err(RuntimeError::TextureNotDepthRenderable(tex.desc.format));
            }
        }

        self.state.color_target = color;
        self.state.depth_stencil = depth_stencil;
        Ok(())
    }

    pub fn set_render_state_u32(&mut self, state_id: u32, value: u32) {
        match state_id {
            // 0: Cull mode (0 none, 1 front, 2 back)
            0 => {
                self.state.cull_mode = match value {
                    1 => CullMode::Front,
                    2 => CullMode::Back,
                    _ => CullMode::None,
                };
            }
            // 1: Depth test enable (0/1)
            1 => self.state.depth_test_enable = value != 0,
            // 2: Depth write enable (0/1)
            2 => self.state.depth_write_enable = value != 0,
            _ => {}
        }
    }

    pub fn create_buffer(
        &mut self,
        buffer_id: u32,
        size: u64,
        usage: u32,
    ) -> Result<(), RuntimeError> {
        if self.buffers.contains_key(&buffer_id) {
            return Err(RuntimeError::BufferAlreadyExists(buffer_id));
        }

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d9-buffer"),
            size,
            usage: map_buffer_usage(usage) | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        self.buffers
            .insert(buffer_id, BufferResource { buffer, size });
        Ok(())
    }

    pub fn write_buffer(
        &mut self,
        buffer_id: u32,
        offset: u64,
        data: &[u8],
    ) -> Result<(), RuntimeError> {
        let buffer = self
            .buffers
            .get(&buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(buffer_id))?;

        let write_end = offset.saturating_add(data.len() as u64);
        if write_end > buffer.size {
            return Err(RuntimeError::BufferWriteOutOfBounds {
                buffer_size: buffer.size,
                write_end,
            });
        }

        self.queue.write_buffer(&buffer.buffer, offset, data);
        Ok(())
    }

    pub fn destroy_buffer(&mut self, buffer_id: u32) -> Result<(), RuntimeError> {
        self.buffers
            .remove(&buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(buffer_id))?;

        if let Some(stream) = self.state.vertex_stream0 {
            if stream.buffer_id == buffer_id {
                self.state.vertex_stream0 = None;
            }
        }
        if let Some(index) = self.state.index_buffer {
            if index.buffer_id == buffer_id {
                self.state.index_buffer = None;
            }
        }
        Ok(())
    }

    pub fn set_render_target_swapchain(&mut self, swapchain_id: u32) -> Result<(), RuntimeError> {
        if !self.swapchains.contains_key(&swapchain_id) {
            return Err(RuntimeError::UnknownSwapChain(swapchain_id));
        }
        self.state.color_target = Some(RenderTarget::SwapChain(swapchain_id));
        self.state.depth_stencil = None;
        Ok(())
    }

    pub fn set_shader_key(&mut self, stage: ShaderStage, key: u32) -> Result<(), RuntimeError> {
        if key != 0 {
            return Err(RuntimeError::UnsupportedShaderKey(key));
        }
        let id = ShaderId(key);
        match stage {
            ShaderStage::Vertex => self.state.vertex_shader = Some(id),
            ShaderStage::Fragment => self.state.fragment_shader = Some(id),
        }
        Ok(())
    }

    pub fn set_constants_f32(
        &mut self,
        stage: ShaderStage,
        start_register: u16,
        vec4_data: &[f32],
    ) -> Result<(), RuntimeError> {
        if stage != ShaderStage::Fragment || start_register != 0 || vec4_data.len() != 4 {
            return Err(RuntimeError::UnsupportedConstantsUpdate {
                stage,
                start_register,
                vec4_count: (vec4_data.len() / 4) as u16,
            });
        }

        self.queue
            .write_buffer(&self.constants_buffer, 0, bytemuck::cast_slice(vec4_data));
        Ok(())
    }

    pub fn set_vertex_decl(&mut self, decl: VertexDecl) -> Result<(), RuntimeError> {
        self.state.vertex_decl = Some(decl);
        Ok(())
    }

    pub fn set_vertex_stream0(
        &mut self,
        buffer_id: u32,
        offset: u64,
        stride: u64,
    ) -> Result<(), RuntimeError> {
        if !self.buffers.contains_key(&buffer_id) {
            return Err(RuntimeError::UnknownBuffer(buffer_id));
        }
        self.state.vertex_stream0 = Some(VertexStreamBinding {
            buffer_id,
            offset,
            stride,
        });
        Ok(())
    }

    pub fn set_index_buffer(
        &mut self,
        buffer_id: u32,
        offset: u64,
        format: IndexFormat,
    ) -> Result<(), RuntimeError> {
        if !self.buffers.contains_key(&buffer_id) {
            return Err(RuntimeError::UnknownBuffer(buffer_id));
        }
        self.state.index_buffer = Some(IndexBinding {
            buffer_id,
            offset,
            format,
        });
        Ok(())
    }

    fn ensure_encoder(&mut self) {
        if self.state.encoder.is_some() {
            return;
        }
        self.state.encoder = Some(self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d9-encoder"),
            },
        ));
        self.state.encoder_needs_clear = true;
    }

    fn next_pass_clear(&mut self) -> bool {
        if self.state.encoder_needs_clear {
            self.state.encoder_needs_clear = false;
            true
        } else {
            false
        }
    }

    pub fn draw(&mut self, vertex_count: u32, first_vertex: u32) -> Result<(), RuntimeError> {
        let color_target = self
            .state
            .color_target
            .ok_or(RuntimeError::MissingRenderTarget)?;
        let color_format = match color_target {
            RenderTarget::SwapChain(id) => {
                self.swapchains
                    .get(&id)
                    .ok_or(RuntimeError::UnknownSwapChain(id))?
                    .desc
                    .format
            }
            RenderTarget::Texture(id) => {
                let tex = self
                    .textures
                    .get(&id)
                    .ok_or(RuntimeError::UnknownTexture(id))?;
                tex.desc
                    .format
                    .as_color()
                    .ok_or(RuntimeError::TextureNotColorRenderable(tex.desc.format))?
            }
        };

        let vs = self
            .state
            .vertex_shader
            .ok_or(RuntimeError::MissingShaders)?;
        let fs = self
            .state
            .fragment_shader
            .ok_or(RuntimeError::MissingShaders)?;
        let mut vertex_decl = self
            .state
            .vertex_decl
            .clone()
            .ok_or(RuntimeError::MissingVertexDeclaration)?;
        let vertex_stream = self
            .state
            .vertex_stream0
            .ok_or(RuntimeError::MissingVertexBuffer)?;
        vertex_decl.stride = vertex_stream.stride;

        let key = PipelineKey {
            vs,
            fs,
            vertex_decl: vertex_decl.clone(),
            color_format,
            cull_mode: self.state.cull_mode,
            has_depth: self.state.depth_stencil.is_some(),
            depth_test_enable: self.state.depth_test_enable,
            depth_write_enable: self.state.depth_write_enable,
        };
        self.ensure_pipeline(&key)?;

        self.ensure_encoder();
        let clear = self.next_pass_clear();
        let color_load = if clear {
            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
        } else {
            wgpu::LoadOp::Load
        };

        let pipeline = self
            .pipelines
            .get(&key)
            .expect("ensure_pipeline inserts pipeline");
        let color_view = match color_target {
            RenderTarget::SwapChain(id) => {
                &self
                    .swapchains
                    .get(&id)
                    .ok_or(RuntimeError::UnknownSwapChain(id))?
                    .view
            }
            RenderTarget::Texture(id) => {
                &self
                    .textures
                    .get(&id)
                    .ok_or(RuntimeError::UnknownTexture(id))?
                    .view_mip0
            }
        };
        let vertex_buffer = &self
            .buffers
            .get(&vertex_stream.buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(vertex_stream.buffer_id))?
            .buffer;
        let depth_attachment = if let Some(depth_id) = self.state.depth_stencil {
            let depth_view = &self
                .textures
                .get(&depth_id)
                .ok_or(RuntimeError::UnknownTexture(depth_id))?
                .view_mip0;
            Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: if clear {
                        wgpu::LoadOp::Clear(1.0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: Some(wgpu::Operations {
                    load: if clear {
                        wgpu::LoadOp::Clear(0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
            })
        } else {
            None
        };

        let encoder = self
            .state
            .encoder
            .as_mut()
            .expect("ensure_encoder initializes encoder");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aero-d3d9-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: color_load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.constants_bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(vertex_stream.offset..));
        pass.draw(first_vertex..first_vertex + vertex_count, 0..1);
        Ok(())
    }

    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        first_index: u32,
        base_vertex: i32,
    ) -> Result<(), RuntimeError> {
        let color_target = self
            .state
            .color_target
            .ok_or(RuntimeError::MissingRenderTarget)?;
        let color_format = match color_target {
            RenderTarget::SwapChain(id) => {
                self.swapchains
                    .get(&id)
                    .ok_or(RuntimeError::UnknownSwapChain(id))?
                    .desc
                    .format
            }
            RenderTarget::Texture(id) => {
                let tex = self
                    .textures
                    .get(&id)
                    .ok_or(RuntimeError::UnknownTexture(id))?;
                tex.desc
                    .format
                    .as_color()
                    .ok_or(RuntimeError::TextureNotColorRenderable(tex.desc.format))?
            }
        };

        let vs = self
            .state
            .vertex_shader
            .ok_or(RuntimeError::MissingShaders)?;
        let fs = self
            .state
            .fragment_shader
            .ok_or(RuntimeError::MissingShaders)?;
        let mut vertex_decl = self
            .state
            .vertex_decl
            .clone()
            .ok_or(RuntimeError::MissingVertexDeclaration)?;
        let vertex_stream = self
            .state
            .vertex_stream0
            .ok_or(RuntimeError::MissingVertexBuffer)?;
        let index_binding = self
            .state
            .index_buffer
            .ok_or(RuntimeError::MissingIndexBuffer)?;
        vertex_decl.stride = vertex_stream.stride;

        let key = PipelineKey {
            vs,
            fs,
            vertex_decl: vertex_decl.clone(),
            color_format,
            cull_mode: self.state.cull_mode,
            has_depth: self.state.depth_stencil.is_some(),
            depth_test_enable: self.state.depth_test_enable,
            depth_write_enable: self.state.depth_write_enable,
        };
        self.ensure_pipeline(&key)?;

        self.ensure_encoder();
        let clear = self.next_pass_clear();
        let color_load = if clear {
            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
        } else {
            wgpu::LoadOp::Load
        };

        let pipeline = self
            .pipelines
            .get(&key)
            .expect("ensure_pipeline inserts pipeline");
        let color_view = match color_target {
            RenderTarget::SwapChain(id) => {
                &self
                    .swapchains
                    .get(&id)
                    .ok_or(RuntimeError::UnknownSwapChain(id))?
                    .view
            }
            RenderTarget::Texture(id) => {
                &self
                    .textures
                    .get(&id)
                    .ok_or(RuntimeError::UnknownTexture(id))?
                    .view_mip0
            }
        };
        let vertex_buffer = &self
            .buffers
            .get(&vertex_stream.buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(vertex_stream.buffer_id))?
            .buffer;
        let index_buffer = &self
            .buffers
            .get(&index_binding.buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(index_binding.buffer_id))?
            .buffer;
        let depth_attachment = if let Some(depth_id) = self.state.depth_stencil {
            let depth_view = &self
                .textures
                .get(&depth_id)
                .ok_or(RuntimeError::UnknownTexture(depth_id))?
                .view_mip0;
            Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: if clear {
                        wgpu::LoadOp::Clear(1.0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: Some(wgpu::Operations {
                    load: if clear {
                        wgpu::LoadOp::Clear(0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
            })
        } else {
            None
        };

        let encoder = self
            .state
            .encoder
            .as_mut()
            .expect("ensure_encoder initializes encoder");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aero-d3d9-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: color_load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.constants_bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(vertex_stream.offset..));
        pass.set_index_buffer(
            index_buffer.slice(index_binding.offset..),
            index_binding.format.to_wgpu(),
        );
        pass.draw_indexed(first_index..first_index + index_count, base_vertex, 0..1);
        Ok(())
    }

    pub fn present(&mut self) -> Result<(), RuntimeError> {
        if let Some(encoder) = self.state.encoder.take() {
            self.queue.submit([encoder.finish()]);
        }
        self.state.encoder_needs_clear = true;
        Ok(())
    }

    pub fn fence_create(&mut self, fence_id: u32) {
        self.fences.entry(fence_id).or_insert(0);
    }

    pub async fn fence_signal(&mut self, fence_id: u32, value: u64) -> Result<(), RuntimeError> {
        self.present()?;
        wait_for_queue(&self.device, &self.queue).await;
        self.fences.insert(fence_id, value);
        Ok(())
    }

    pub async fn fence_wait(&mut self, fence_id: u32, value: u64) -> Result<(), RuntimeError> {
        loop {
            let current = *self.fences.get(&fence_id).unwrap_or(&0);
            if current >= value {
                return Ok(());
            }
            wait_for_queue(&self.device, &self.queue).await;
        }
    }

    pub fn fence_destroy(&mut self, fence_id: u32) {
        self.fences.remove(&fence_id);
    }

    pub async fn readback_swapchain_rgba8(
        &self,
        swapchain_id: u32,
    ) -> Result<(u32, u32, Vec<u8>), RuntimeError> {
        let swapchain = self
            .swapchains
            .get(&swapchain_id)
            .ok_or(RuntimeError::UnknownSwapChain(swapchain_id))?;

        let wgpu_format = swapchain.desc.format.to_wgpu();
        match wgpu_format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {}
            other => return Err(RuntimeError::UnsupportedReadbackFormat(other)),
        }

        let bytes_per_pixel = 4u32;
        let width = swapchain.desc.width;
        let height = swapchain.desc.height;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let padded_bytes_per_row =
            align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d9-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d9-readback-encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &swapchain.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
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

        let slice = readback.slice(..);
        let (sender, receiver) = oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result.map_err(|e| e.to_string()));
        });

        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        let mapped = receiver
            .receive()
            .await
            .ok_or(RuntimeError::MapAsyncDropped)?;
        mapped.map_err(RuntimeError::MapAsync)?;

        let data = slice.get_mapped_range();
        let mut pixels = vec![0u8; (width * height * bytes_per_pixel) as usize];
        for y in 0..height as usize {
            let src = y * padded_bytes_per_row as usize;
            let dst = y * unpadded_bytes_per_row as usize;
            pixels[dst..dst + unpadded_bytes_per_row as usize]
                .copy_from_slice(&data[src..src + unpadded_bytes_per_row as usize]);
        }

        drop(data);
        readback.unmap();
        Ok((width, height, pixels))
    }

    pub async fn readback_texture_rgba8(
        &self,
        texture_id: u32,
    ) -> Result<(u32, u32, Vec<u8>), RuntimeError> {
        let texture = self
            .textures
            .get(&texture_id)
            .ok_or(RuntimeError::UnknownTexture(texture_id))?;

        let wgpu_format = texture.desc.format.to_wgpu();
        match wgpu_format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {}
            other => return Err(RuntimeError::UnsupportedReadbackFormat(other)),
        }

        let bytes_per_pixel = 4u32;
        let width = texture.desc.width;
        let height = texture.desc.height;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let padded_bytes_per_row =
            align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d9-texture-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d9-texture-readback-encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
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

        let slice = readback.slice(..);
        let (sender, receiver) = oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result.map_err(|e| e.to_string()));
        });

        self.device.poll(wgpu::Maintain::Wait);
        let result = receiver.receive().await.ok_or(RuntimeError::MapAsyncDropped)?;
        result.map_err(RuntimeError::MapAsync)?;

        let data = slice.get_mapped_range();
        let mut pixels = vec![0u8; (width * height * bytes_per_pixel) as usize];
        for y in 0..height as usize {
            let src = y * padded_bytes_per_row as usize;
            let dst = y * unpadded_bytes_per_row as usize;
            pixels[dst..dst + unpadded_bytes_per_row as usize]
                .copy_from_slice(&data[src..src + unpadded_bytes_per_row as usize]);
        }

        drop(data);
        readback.unmap();
        Ok((width, height, pixels))
    }

    fn ensure_pipeline(&mut self, key: &PipelineKey) -> Result<(), RuntimeError> {
        if self.pipelines.contains_key(key) {
            return Ok(());
        }

        self.ensure_builtin_module()?;
        let module = self
            .builtin_shader_module
            .as_ref()
            .expect("ensure_builtin_module initializes module");
        let mut attributes = Vec::with_capacity(key.vertex_decl.attributes.len());
        for attr in &key.vertex_decl.attributes {
            attributes.push(wgpu::VertexAttribute {
                format: attr.format.to_wgpu(),
                offset: attr.offset as u64,
                shader_location: attr.location,
            });
        }

        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aero-d3d9-pipeline"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: "vs_main",
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: key.vertex_decl.stride,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    }],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: match key.cull_mode {
                        CullMode::None => None,
                        CullMode::Front => Some(wgpu::Face::Front),
                        CullMode::Back => Some(wgpu::Face::Back),
                    },
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: if key.has_depth {
                    Some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth24PlusStencil8,
                        depth_write_enabled: key.depth_write_enable,
                        depth_compare: if key.depth_test_enable {
                            wgpu::CompareFunction::LessEqual
                        } else {
                            wgpu::CompareFunction::Always
                        },
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    })
                } else {
                    None
                },
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: key.color_format.to_wgpu(),
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview: None,
            });

        self.pipelines.insert(key.clone(), pipeline);
        Ok(())
    }

    fn ensure_builtin_module(&mut self) -> Result<(), RuntimeError> {
        if self.builtin_shader_module.is_some() {
            return Ok(());
        }

        let wgsl = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
}

struct VsOut {
    @builtin(position) position: vec4<f32>,
}

struct Constants {
    color: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> constants: Constants;

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.position = vec4<f32>(input.pos, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(_input: VsOut) -> @location(0) vec4<f32> {
    return constants.color;
}
"#;
        self.builtin_shader_module = Some(self.device.create_shader_module(
            wgpu::ShaderModuleDescriptor {
                label: Some("aero-d3d9-builtin-shader"),
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
            },
        ));
        Ok(())
    }
}

fn align_to(value: u32, alignment: u32) -> u32 {
    let mask = alignment - 1;
    (value + mask) & !mask
}

fn map_buffer_usage(bits: u32) -> wgpu::BufferUsages {
    const VERTEX: u32 = 1 << 0;
    const INDEX: u32 = 1 << 1;
    const UNIFORM: u32 = 1 << 2;

    let mut out = wgpu::BufferUsages::empty();
    if (bits & VERTEX) != 0 {
        out |= wgpu::BufferUsages::VERTEX;
    }
    if (bits & INDEX) != 0 {
        out |= wgpu::BufferUsages::INDEX;
    }
    if (bits & UNIFORM) != 0 {
        out |= wgpu::BufferUsages::UNIFORM;
    }
    out
}

fn map_texture_usage(bits: u32) -> wgpu::TextureUsages {
    const SAMPLED: u32 = 1 << 0;
    const RENDER_TARGET: u32 = 1 << 1;
    const DEPTH_STENCIL: u32 = 1 << 2;

    let mut out = wgpu::TextureUsages::empty();
    if (bits & SAMPLED) != 0 {
        out |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    if (bits & RENDER_TARGET) != 0 {
        out |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    if (bits & DEPTH_STENCIL) != 0 {
        out |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    out
}

fn pad_rows(
    src: &[u8],
    unpadded_bytes_per_row: usize,
    padded_bytes_per_row: usize,
    height: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; padded_bytes_per_row * height];
    for y in 0..height {
        let src_row = y * unpadded_bytes_per_row;
        let dst_row = y * padded_bytes_per_row;
        out[dst_row..dst_row + unpadded_bytes_per_row]
            .copy_from_slice(&src[src_row..src_row + unpadded_bytes_per_row]);
    }
    out
}

async fn wait_for_queue(device: &wgpu::Device, queue: &wgpu::Queue) {
    let (sender, receiver) = oneshot_channel();
    queue.on_submitted_work_done(move || {
        let _ = sender.send(());
    });
    // wgpu only dispatches `on_submitted_work_done` callbacks while polling the device.
    // Without an explicit poll this future can deadlock on native backends.
    device.poll(wgpu::Maintain::Wait);
    let _ = receiver.receive().await;
}
