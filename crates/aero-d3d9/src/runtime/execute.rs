use std::collections::HashMap;

use futures_intrusive::channel::shared::oneshot_channel;
use thiserror::Error;
use tracing::debug;
use wgpu::util::DeviceExt;

use crate::state::{
    topology::D3DPrimitiveType,
    tracker::{
        BlendFactor, BlendOp, ColorWriteMask, CompareFunc, CullMode, ScissorRect, ShaderKey,
        StencilOp, VertexAttributeKey, VertexBufferLayoutKey, Viewport,
    },
    translate_pipeline_state, PipelineCache, PipelineKey,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeConfig {
    pub validation: bool,
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
    view_srgb: Option<wgpu::TextureView>,
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
    view_mip0_srgb: Option<wgpu::TextureView>,
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

const MAX_SAMPLERS: usize = 16;
const MAX_REASONABLE_RENDER_STATE_ID: u32 = 4096;
const MAX_REASONABLE_SAMPLER_STATE_ID: u32 = 4096;

const DISABLE_WGPU_TEXTURE_COMPRESSION_ENV: &str = "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION";

fn env_var_truthy(name: &str) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

fn negotiated_features_for_available(
    available: wgpu::Features,
    backend_is_gl: bool,
    disable_texture_compression: bool,
) -> wgpu::Features {
    let mut requested = wgpu::Features::empty();

    // wgpu's GL backend has had correctness issues with native BC textures on some platforms
    // (notably Linux CI software adapters). Treat compression as disabled regardless of adapter
    // feature bits to keep tests deterministic.
    if !disable_texture_compression && !backend_is_gl {
        // Texture compression is optional but beneficial (guest textures, DDS, etc).
        for feature in [
            wgpu::Features::TEXTURE_COMPRESSION_BC,
            wgpu::Features::TEXTURE_COMPRESSION_ETC2,
            wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR,
        ] {
            if available.contains(feature) {
                requested |= feature;
            }
        }
    }

    requested
}

#[derive(Debug)]
struct GraphicsState {
    color_target: Option<RenderTarget>,
    depth_stencil: Option<u32>,
    vertex_decl: Option<VertexDecl>,
    vertex_stream0: Option<VertexStreamBinding>,
    index_buffer: Option<IndexBinding>,
    primitive_type: D3DPrimitiveType,
    scissor_enable: bool,
    scissor_rect: Option<ScissorRect>,
    viewport: Option<Viewport>,
    encoder: Option<wgpu::CommandEncoder>,
    encoder_needs_clear: bool,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            color_target: None,
            depth_stencil: None,
            vertex_decl: None,
            vertex_stream0: None,
            index_buffer: None,
            primitive_type: D3DPrimitiveType::TriangleList,
            scissor_enable: false,
            scissor_rect: None,
            viewport: None,
            encoder: None,
            encoder_needs_clear: true,
        }
    }
}

pub struct D3D9Runtime {
    config: RuntimeConfig,
    device: wgpu::Device,
    queue: wgpu::Queue,
    downlevel_flags: wgpu::DownlevelFlags,

    buffers: HashMap<u32, BufferResource>,
    swapchains: HashMap<u32, SwapChainResource>,
    textures: HashMap<u32, TextureResource>,

    builtin_shader_module: Option<wgpu::ShaderModule>,
    pipelines: PipelineCache,

    constants_buffer: wgpu::Buffer,
    constants_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    texture_bind_group: Option<wgpu::BindGroup>,
    texture_bind_group_dirty: bool,
    pipeline_layout: wgpu::PipelineLayout,

    default_texture_view: wgpu::TextureView,
    default_sampler: wgpu::Sampler,
    sampler_cache: [wgpu::Sampler; MAX_SAMPLERS],
    bound_textures: [Option<u32>; MAX_SAMPLERS],

    tracker: crate::state::StateTracker,
    render_states: Vec<u32>,
    sampler_states: [Vec<u32>; MAX_SAMPLERS],

    state: GraphicsState,
    fences: HashMap<u32, u64>,
}

impl D3D9Runtime {
    pub async fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        #[cfg(all(unix, not(target_arch = "wasm32")))]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);
            if needs_runtime_dir {
                let dir = std::env::temp_dir().join(format!(
                    "aero-d3d9-xdg-runtime-{}-microtests",
                    std::process::id()
                ));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
                wgpu::Backends::all()
            },
            ..Default::default()
        });
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
        {
            Some(adapter) => adapter,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                })
                .await
                .ok_or(RuntimeError::AdapterNotFound)?,
        };

        let downlevel_flags = adapter.get_downlevel_capabilities().flags;

        let backend_is_gl = adapter.get_info().backend == wgpu::Backend::Gl;

        let adapter_features = adapter.features();
        let texture_compression_disabled_by_env =
            env_var_truthy(DISABLE_WGPU_TEXTURE_COMPRESSION_ENV);
        let texture_compression_disabled = texture_compression_disabled_by_env || backend_is_gl;
        let required_features = negotiated_features_for_available(
            adapter_features,
            backend_is_gl,
            texture_compression_disabled_by_env,
        );

        debug!(
            ?adapter_features,
            ?required_features,
            texture_compression_disabled,
            backend_is_gl,
            "aero-d3d9 negotiated wgpu features"
        );

        let required_limits = if cfg!(target_os = "linux") {
            wgpu::Limits::downlevel_defaults()
        } else {
            wgpu::Limits::default()
        };
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("aero-d3d9-device"),
            required_features,
            required_limits,
        };

        let (device, queue) = adapter
            .request_device(&descriptor, None)
            .await
            .map_err(|e| RuntimeError::RequestDevice(e.to_string()))?;

        let device_features = device.features();
        debug!(?device_features, "aero-d3d9 created wgpu device");

        debug_assert!(device_features.contains(required_features));

        // Regression check: if the adapter supports BC and it's not disabled, ensure we actually
        // requested it so DXT textures can remain compressed on the GPU.
        if !texture_compression_disabled
            && adapter_features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            debug_assert!(
                device_features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
                "adapter supports TEXTURE_COMPRESSION_BC but aero-d3d9 device was not created with it (set AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1 to force opt-out)"
            );
        }

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

        let mut texture_entries = Vec::with_capacity(MAX_SAMPLERS * 2);
        for slot in 0..MAX_SAMPLERS {
            texture_entries.push(wgpu::BindGroupLayoutEntry {
                binding: (slot * 2) as u32,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
            texture_entries.push(wgpu::BindGroupLayoutEntry {
                binding: (slot * 2 + 1) as u32,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            });
        }

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("aero-d3d9-textures-bgl"),
                entries: &texture_entries,
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d9-pipeline-layout"),
            bind_group_layouts: &[&constants_bind_group_layout, &texture_bind_group_layout],
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

        let default_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d9-default-texture"),
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
                texture: &default_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0u8, 0, 0, 0],
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
        let default_texture_view =
            default_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let default_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aero-d3d9-default-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let sampler_cache = std::array::from_fn(|_| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("aero-d3d9-sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                address_mode_w: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            })
        });

        Ok(Self {
            config,
            device,
            queue,
            downlevel_flags,
            buffers: HashMap::new(),
            swapchains: HashMap::new(),
            textures: HashMap::new(),
            builtin_shader_module: None,
            pipelines: PipelineCache::new(256),
            constants_buffer,
            constants_bind_group,
            texture_bind_group_layout,
            texture_bind_group: None,
            texture_bind_group_dirty: true,
            pipeline_layout,
            default_texture_view,
            default_sampler,
            sampler_cache,
            bound_textures: [None; MAX_SAMPLERS],
            tracker: crate::state::StateTracker::default(),
            render_states: Vec::new(),
            sampler_states: std::array::from_fn(|_| Vec::new()),
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
        let view_formats = if self
            .downlevel_flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
        {
            match format {
                wgpu::TextureFormat::Rgba8Unorm => vec![wgpu::TextureFormat::Rgba8UnormSrgb],
                wgpu::TextureFormat::Bgra8Unorm => vec![wgpu::TextureFormat::Bgra8UnormSrgb],
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
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
            view_formats: &view_formats,
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let view_srgb = if self
            .downlevel_flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
        {
            match format {
                wgpu::TextureFormat::Rgba8Unorm => {
                    Some(texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("aero-d3d9-swapchain-view-srgb"),
                        format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                        ..Default::default()
                    }))
                }
                wgpu::TextureFormat::Bgra8Unorm => {
                    Some(texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("aero-d3d9-swapchain-view-srgb"),
                        format: Some(wgpu::TextureFormat::Bgra8UnormSrgb),
                        ..Default::default()
                    }))
                }
                _ => None,
            }
        } else {
            None
        };

        self.swapchains.insert(
            swapchain_id,
            SwapChainResource {
                desc,
                texture,
                view,
                view_srgb,
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
        let view_formats = if self
            .downlevel_flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
        {
            match format {
                wgpu::TextureFormat::Rgba8Unorm => vec![wgpu::TextureFormat::Rgba8UnormSrgb],
                wgpu::TextureFormat::Bgra8Unorm => vec![wgpu::TextureFormat::Bgra8UnormSrgb],
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
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
            view_formats: &view_formats,
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
        let view_mip0_srgb = if self
            .downlevel_flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
        {
            match format {
                wgpu::TextureFormat::Rgba8Unorm => {
                    Some(texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("aero-d3d9-texture-mip0-srgb"),
                        base_mip_level: 0,
                        mip_level_count: Some(1),
                        base_array_layer: 0,
                        array_layer_count: Some(1),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                        aspect: wgpu::TextureAspect::All,
                    }))
                }
                wgpu::TextureFormat::Bgra8Unorm => {
                    Some(texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("aero-d3d9-texture-mip0-srgb"),
                        base_mip_level: 0,
                        mip_level_count: Some(1),
                        base_array_layer: 0,
                        array_layer_count: Some(1),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        format: Some(wgpu::TextureFormat::Bgra8UnormSrgb),
                        aspect: wgpu::TextureAspect::All,
                    }))
                }
                _ => None,
            }
        } else {
            None
        };

        self.textures.insert(
            texture_id,
            TextureResource {
                desc,
                texture,
                view_mip0,
                view_mip0_srgb,
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
        let desc = self
            .textures
            .get(&texture_id)
            .ok_or(RuntimeError::UnknownTexture(texture_id))?
            .desc;

        let expected_width = (desc.width >> mip_level).max(1);
        let expected_height = (desc.height >> mip_level).max(1);
        if width != expected_width || height != expected_height {
            return Err(RuntimeError::TextureUpdateDimensionsMismatch {
                mip_level,
                expected_width,
                expected_height,
                provided_width: width,
                provided_height: height,
            });
        }

        if desc.format.as_color().is_none() {
            return Err(RuntimeError::UnsupportedTextureUpdateFormat(desc.format));
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

        self.submit_encoder_for_queue_write();

        let texture = self
            .textures
            .get(&texture_id)
            .ok_or(RuntimeError::UnknownTexture(texture_id))?;

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

        for slot in 0..MAX_SAMPLERS {
            if self.bound_textures[slot] == Some(texture_id) {
                self.bound_textures[slot] = None;
                self.tracker.textures[slot] = None;
                self.texture_bind_group_dirty = true;
            }
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

        let mut color_formats = Vec::new();
        if let Some(target) = self.state.color_target {
            let fmt = match target {
                RenderTarget::SwapChain(id) => self
                    .swapchains
                    .get(&id)
                    .ok_or(RuntimeError::UnknownSwapChain(id))?
                    .desc
                    .format
                    .to_wgpu(),
                RenderTarget::Texture(id) => self
                    .textures
                    .get(&id)
                    .ok_or(RuntimeError::UnknownTexture(id))?
                    .desc
                    .format
                    .to_wgpu(),
            };
            color_formats.push(fmt);
        }
        let depth_format = if self.state.depth_stencil.is_some() {
            Some(wgpu::TextureFormat::Depth24PlusStencil8)
        } else {
            None
        };
        self.tracker.set_render_targets(color_formats, depth_format);

        Ok(())
    }

    pub fn set_render_state_u32(&mut self, state_id: u32, value: u32) {
        if state_id > MAX_REASONABLE_RENDER_STATE_ID {
            debug!(
                state_id,
                value, "ignoring suspiciously large D3D9 render state id"
            );
            return;
        }

        let idx = state_id as usize;
        if idx >= self.render_states.len() {
            self.render_states.resize(idx + 1, 0);
        }
        if self.render_states[idx] == value {
            return;
        }
        self.render_states[idx] = value;

        match state_id {
            d3d9::D3DRS_ZENABLE => self.tracker.depth_stencil.depth_enable = value != 0,
            d3d9::D3DRS_ZWRITEENABLE => self.tracker.depth_stencil.depth_write_enable = value != 0,
            d3d9::D3DRS_ZFUNC => match d3d9_compare_func(value) {
                Some(func) => self.tracker.depth_stencil.depth_func = func,
                None => debug!(state_id, value, "unknown D3D9 compare func"),
            },
            d3d9::D3DRS_FRONTCOUNTERCLOCKWISE => {
                self.tracker.rasterizer.front_counter_clockwise = value != 0
            }
            d3d9::D3DRS_STENCILENABLE => self.tracker.depth_stencil.stencil_enable = value != 0,
            d3d9::D3DRS_STENCILFUNC => match d3d9_compare_func(value) {
                Some(func) => self.tracker.depth_stencil.stencil_func = func,
                None => debug!(state_id, value, "unknown D3D9 compare func"),
            },
            d3d9::D3DRS_STENCILFAIL => match d3d9_stencil_op(value) {
                Some(op) => self.tracker.depth_stencil.stencil_fail = op,
                None => debug!(state_id, value, "unknown D3D9 stencil op"),
            },
            d3d9::D3DRS_STENCILZFAIL => match d3d9_stencil_op(value) {
                Some(op) => self.tracker.depth_stencil.stencil_zfail = op,
                None => debug!(state_id, value, "unknown D3D9 stencil op"),
            },
            d3d9::D3DRS_STENCILPASS => match d3d9_stencil_op(value) {
                Some(op) => self.tracker.depth_stencil.stencil_pass = op,
                None => debug!(state_id, value, "unknown D3D9 stencil op"),
            },
            d3d9::D3DRS_STENCILREF => self.tracker.depth_stencil.stencil_ref = (value & 0xFF) as u8,
            d3d9::D3DRS_STENCILMASK => {
                self.tracker.depth_stencil.stencil_read_mask = (value & 0xFF) as u8
            }
            d3d9::D3DRS_STENCILWRITEMASK => {
                self.tracker.depth_stencil.stencil_write_mask = (value & 0xFF) as u8
            }
            d3d9::D3DRS_ALPHABLENDENABLE => self.tracker.blend.alpha_blend_enable = value != 0,
            d3d9::D3DRS_SRCBLEND => match value {
                d3d9::D3DBLEND_BOTHSRCALPHA => {
                    self.tracker.blend.src_blend = BlendFactor::SrcAlpha;
                    self.tracker.blend.dst_blend = BlendFactor::InvSrcAlpha;
                }
                d3d9::D3DBLEND_BOTHINVSRCALPHA => {
                    self.tracker.blend.src_blend = BlendFactor::InvSrcAlpha;
                    self.tracker.blend.dst_blend = BlendFactor::SrcAlpha;
                }
                _ => match d3d9_blend_factor(value) {
                    Some(f) => self.tracker.blend.src_blend = f,
                    None => debug!(state_id, value, "unknown D3D9 blend factor"),
                },
            },
            d3d9::D3DRS_DESTBLEND => match d3d9_blend_factor(value) {
                Some(f) => self.tracker.blend.dst_blend = f,
                None => debug!(state_id, value, "unknown D3D9 blend factor"),
            },
            d3d9::D3DRS_BLENDOP => match d3d9_blend_op(value) {
                Some(op) => self.tracker.blend.blend_op = op,
                None => debug!(state_id, value, "unknown D3D9 blend op"),
            },
            d3d9::D3DRS_SEPARATEALPHABLENDENABLE => {
                self.tracker.blend.separate_alpha_blend_enable = value != 0
            }
            d3d9::D3DRS_SRCBLENDALPHA => match d3d9_blend_factor(value) {
                Some(f) => self.tracker.blend.src_blend_alpha = f,
                None => debug!(state_id, value, "unknown D3D9 blend factor"),
            },
            d3d9::D3DRS_DESTBLENDALPHA => match d3d9_blend_factor(value) {
                Some(f) => self.tracker.blend.dst_blend_alpha = f,
                None => debug!(state_id, value, "unknown D3D9 blend factor"),
            },
            d3d9::D3DRS_BLENDOPALPHA => match d3d9_blend_op(value) {
                Some(op) => self.tracker.blend.blend_op_alpha = op,
                None => debug!(state_id, value, "unknown D3D9 blend op"),
            },
            d3d9::D3DRS_BLENDFACTOR => self.tracker.blend.blend_factor = value,
            d3d9::D3DRS_CULLMODE => match d3d9_cull_mode(value) {
                Some(mode) => self.tracker.rasterizer.cull_mode = mode,
                None => debug!(state_id, value, "unknown D3D9 cull mode"),
            },
            d3d9::D3DRS_COLORWRITEENABLE => {
                self.tracker
                    .set_color_write_mask(0, ColorWriteMask((value & 0xF) as u8));
            }
            d3d9::D3DRS_COLORWRITEENABLE1 => {
                self.tracker
                    .set_color_write_mask(1, ColorWriteMask((value & 0xF) as u8));
            }
            d3d9::D3DRS_COLORWRITEENABLE2 => {
                self.tracker
                    .set_color_write_mask(2, ColorWriteMask((value & 0xF) as u8));
            }
            d3d9::D3DRS_COLORWRITEENABLE3 => {
                self.tracker
                    .set_color_write_mask(3, ColorWriteMask((value & 0xF) as u8));
            }
            d3d9::D3DRS_SRGBWRITEENABLE => self.tracker.set_srgb_write_enable(value != 0),
            d3d9::D3DRS_SCISSORTESTENABLE => self.state.scissor_enable = value != 0,
            _ => debug!(state_id, value, "unhandled D3D9 render state"),
        }
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.state.viewport = Some(viewport);
        self.tracker.set_viewport(viewport);
    }

    pub fn set_scissor_rect(&mut self, rect: ScissorRect) {
        self.state.scissor_rect = Some(rect);
        self.tracker.set_scissor_rect(rect);
    }

    pub fn set_texture(
        &mut self,
        stage: ShaderStage,
        slot: u32,
        texture_id: Option<u32>,
    ) -> Result<(), RuntimeError> {
        if stage != ShaderStage::Fragment {
            debug!(
                ?stage,
                slot, texture_id, "ignoring non-fragment texture bind"
            );
            return Ok(());
        }

        let slot_usize = slot as usize;
        if slot_usize >= MAX_SAMPLERS {
            debug!(slot, texture_id, "ignoring out-of-range texture slot");
            return Ok(());
        }

        if let Some(id) = texture_id {
            if !self.textures.contains_key(&id) {
                return Err(RuntimeError::UnknownTexture(id));
            }
        }

        if self.bound_textures[slot_usize] == texture_id {
            return Ok(());
        }

        self.bound_textures[slot_usize] = texture_id;
        self.tracker.textures[slot_usize] = texture_id.map(|v| v as u64);
        self.texture_bind_group_dirty = true;
        Ok(())
    }

    pub fn set_sampler_state_u32(
        &mut self,
        stage: ShaderStage,
        slot: u32,
        state_id: u32,
        value: u32,
    ) {
        if stage != ShaderStage::Fragment {
            debug!(
                ?stage,
                slot, state_id, value, "ignoring non-fragment sampler state"
            );
            return;
        }

        if state_id > MAX_REASONABLE_SAMPLER_STATE_ID {
            debug!(
                slot,
                state_id, value, "ignoring suspiciously large D3D9 sampler state id"
            );
            return;
        }

        let slot_usize = slot as usize;
        if slot_usize >= MAX_SAMPLERS {
            debug!(slot, state_id, value, "ignoring out-of-range sampler slot");
            return;
        }

        let table = &mut self.sampler_states[slot_usize];
        let idx = state_id as usize;
        if idx >= table.len() {
            table.resize(idx + 1, 0);
        }
        if table[idx] == value {
            return;
        }
        table[idx] = value;

        match state_id {
            d3d9::D3DSAMP_ADDRESSU => self.tracker.samplers[slot_usize].address_u = value,
            d3d9::D3DSAMP_ADDRESSV => self.tracker.samplers[slot_usize].address_v = value,
            d3d9::D3DSAMP_MINFILTER => self.tracker.samplers[slot_usize].min_filter = value,
            d3d9::D3DSAMP_MAGFILTER => self.tracker.samplers[slot_usize].mag_filter = value,
            d3d9::D3DSAMP_MIPFILTER => self.tracker.samplers[slot_usize].mip_filter = value,
            _ => debug!(slot, state_id, value, "unhandled D3D9 sampler state"),
        }

        self.sampler_cache[slot_usize] = create_wgpu_sampler(
            &self.device,
            &self.tracker.samplers[slot_usize],
            &self.default_sampler,
        );
        self.texture_bind_group_dirty = true;
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
        let buffer_size = self
            .buffers
            .get(&buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(buffer_id))?
            .size;

        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        let size_bytes = data.len() as u64;
        if !offset.is_multiple_of(alignment) || !size_bytes.is_multiple_of(alignment) {
            return Err(RuntimeError::Validation(format!(
                "buffer writes must be {alignment}-byte aligned (offset={offset} size_bytes={size_bytes})"
            )));
        }

        let write_end = offset.saturating_add(data.len() as u64);
        if write_end > buffer_size {
            return Err(RuntimeError::BufferWriteOutOfBounds {
                buffer_size,
                write_end,
            });
        }

        self.submit_encoder_for_queue_write();

        let buffer = self
            .buffers
            .get(&buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(buffer_id))?;
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
        self.set_render_targets(Some(RenderTarget::SwapChain(swapchain_id)), None)
    }

    pub fn set_shader_key(&mut self, stage: ShaderStage, key: u32) -> Result<(), RuntimeError> {
        // Key 0 is treated as "unbind", matching D3D9 semantics.
        let shader = if key == 0 {
            None
        } else {
            Some(ShaderKey(key as u64))
        };

        match stage {
            ShaderStage::Vertex => {
                // For now we only expose a single built-in vertex shader under key=1.
                if shader.is_some() && key != 1 {
                    return Err(RuntimeError::UnsupportedShaderKey(key));
                }
                self.tracker.set_vertex_shader(shader);
            }
            ShaderStage::Fragment => {
                // Fragment shaders: 1 = solid color, 2 = textured.
                if shader.is_some() && key != 1 && key != 2 {
                    return Err(RuntimeError::UnsupportedShaderKey(key));
                }
                self.tracker.set_pixel_shader(shader);
            }
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

        // Encode the update into the current command encoder so ordering with draws is preserved.
        self.ensure_encoder();
        let staging = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aero-d3d9-constants-staging"),
                contents: bytemuck::cast_slice(vec4_data),
                usage: wgpu::BufferUsages::COPY_SRC,
            });
        let encoder = self
            .state
            .encoder
            .as_mut()
            .expect("ensure_encoder initializes encoder");
        encoder.copy_buffer_to_buffer(&staging, 0, &self.constants_buffer, 0, 16);
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
    }

    fn submit_encoder_for_queue_write(&mut self) {
        // `wgpu::Queue::write_buffer` / `write_texture` are executed immediately relative to
        // subsequent `queue.submit()` calls. If we have pending GPU work recorded into an unsent
        // command encoder, a mid-stream upload could otherwise be reordered ahead of earlier draws.
        //
        // To preserve D3D9 stream ordering we flush the current encoder before performing uploads.
        if let Some(encoder) = self.state.encoder.take() {
            self.queue.submit([encoder.finish()]);
        }
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

        if self.tracker.vertex_shader.is_none() || self.tracker.pixel_shader.is_none() {
            return Err(RuntimeError::MissingShaders);
        }

        let decl = self
            .state
            .vertex_decl
            .clone()
            .ok_or(RuntimeError::MissingVertexDeclaration)?;
        let vertex_stream = self
            .state
            .vertex_stream0
            .ok_or(RuntimeError::MissingVertexBuffer)?;

        // Update the tracker with the current vertex layout. In D3D9 the vertex declaration
        // does not include the stream stride, so we take it from the stream binding.
        let layout = VertexBufferLayoutKey {
            array_stride: vertex_stream.stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: decl
                .attributes
                .iter()
                .map(|attr| VertexAttributeKey {
                    format: attr.format.to_wgpu(),
                    offset: attr.offset as u64,
                    shader_location: attr.location,
                })
                .collect(),
        };
        self.tracker.set_vertex_layouts(vec![layout]);
        self.tracker.set_primitive_type(self.state.primitive_type);

        let (pipeline_key, translated, dynamic) =
            translate_pipeline_state(&self.tracker).ok_or(RuntimeError::MissingShaders)?;

        let (vs_entry, fs_entry) = builtin_entry_points(&pipeline_key)?;

        self.ensure_builtin_module()?;
        self.ensure_texture_bind_group();
        self.ensure_encoder();

        let key_for_create = pipeline_key.clone();
        let translated_for_create = translated.clone();
        let module = self
            .builtin_shader_module
            .as_ref()
            .expect("ensure_builtin_module initializes module");
        let device = &self.device;
        let pipeline_layout = &self.pipeline_layout;
        let pipeline = self.pipelines.get_or_create(pipeline_key, || {
            create_render_pipeline(
                device,
                pipeline_layout,
                module,
                &key_for_create,
                &translated_for_create,
                vs_entry,
                fs_entry,
            )
        });

        let clear = self.next_pass_clear();
        let color_load = if clear {
            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
        } else {
            wgpu::LoadOp::Load
        };

        let wants_srgb = self.tracker.render_targets.srgb_write_enable;
        let (color_view, target_width, target_height) =
            resolve_color_target_view(color_target, wants_srgb, &self.swapchains, &self.textures)?;

        let vertex_buffer = &self
            .buffers
            .get(&vertex_stream.buffer_id)
            .ok_or(RuntimeError::UnknownBuffer(vertex_stream.buffer_id))?
            .buffer;

        let depth_attachment = if translated.depth_stencil.is_some() {
            if let Some(depth_id) = self.state.depth_stencil {
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
            }
        } else {
            None
        };

        // Apply dynamic viewport/scissor state. Ensure we always reset to a sensible default so
        // state doesn't leak between draws.
        let viewport = dynamic.viewport.unwrap_or(Viewport {
            x: 0.0,
            y: 0.0,
            width: target_width as f32,
            height: target_height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        });
        let viewport = clamp_viewport(viewport, target_width, target_height);

        let scissor = if self.state.scissor_enable {
            dynamic.scissor
        } else {
            None
        };
        let scissor = match scissor {
            Some(rect) => clamp_scissor(rect, target_width, target_height),
            None => Some(ScissorRect {
                x: 0,
                y: 0,
                width: target_width,
                height: target_height,
            }),
        };
        if scissor.is_none() {
            // Empty scissor; skip draw without producing validation errors.
            return Ok(());
        }

        let texture_bind_group = self
            .texture_bind_group
            .as_ref()
            .expect("ensure_texture_bind_group initializes bind group");

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

        pass.set_pipeline(pipeline.as_ref());
        pass.set_bind_group(0, &self.constants_bind_group, &[]);
        pass.set_bind_group(1, texture_bind_group, &[]);
        pass.set_blend_constant(dynamic.blend_constant);
        pass.set_stencil_reference(dynamic.stencil_reference);
        pass.set_viewport(
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            viewport.min_depth,
            viewport.max_depth,
        );
        if let Some(rect) = scissor {
            pass.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
        }

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

        if self.tracker.vertex_shader.is_none() || self.tracker.pixel_shader.is_none() {
            return Err(RuntimeError::MissingShaders);
        }

        let decl = self
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

        let layout = VertexBufferLayoutKey {
            array_stride: vertex_stream.stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: decl
                .attributes
                .iter()
                .map(|attr| VertexAttributeKey {
                    format: attr.format.to_wgpu(),
                    offset: attr.offset as u64,
                    shader_location: attr.location,
                })
                .collect(),
        };
        self.tracker.set_vertex_layouts(vec![layout]);
        self.tracker.set_primitive_type(self.state.primitive_type);

        let (pipeline_key, translated, dynamic) =
            translate_pipeline_state(&self.tracker).ok_or(RuntimeError::MissingShaders)?;

        let (vs_entry, fs_entry) = builtin_entry_points(&pipeline_key)?;

        self.ensure_builtin_module()?;
        self.ensure_texture_bind_group();
        self.ensure_encoder();

        let key_for_create = pipeline_key.clone();
        let translated_for_create = translated.clone();
        let module = self
            .builtin_shader_module
            .as_ref()
            .expect("ensure_builtin_module initializes module");
        let device = &self.device;
        let pipeline_layout = &self.pipeline_layout;
        let pipeline = self.pipelines.get_or_create(pipeline_key, || {
            create_render_pipeline(
                device,
                pipeline_layout,
                module,
                &key_for_create,
                &translated_for_create,
                vs_entry,
                fs_entry,
            )
        });

        let clear = self.next_pass_clear();
        let color_load = if clear {
            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
        } else {
            wgpu::LoadOp::Load
        };

        let wants_srgb = self.tracker.render_targets.srgb_write_enable;
        let (color_view, target_width, target_height) =
            resolve_color_target_view(color_target, wants_srgb, &self.swapchains, &self.textures)?;

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

        let depth_attachment = if translated.depth_stencil.is_some() {
            if let Some(depth_id) = self.state.depth_stencil {
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
            }
        } else {
            None
        };

        let viewport = dynamic.viewport.unwrap_or(Viewport {
            x: 0.0,
            y: 0.0,
            width: target_width as f32,
            height: target_height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        });
        let viewport = clamp_viewport(viewport, target_width, target_height);

        let scissor = if self.state.scissor_enable {
            dynamic.scissor
        } else {
            None
        };
        let scissor = match scissor {
            Some(rect) => clamp_scissor(rect, target_width, target_height),
            None => Some(ScissorRect {
                x: 0,
                y: 0,
                width: target_width,
                height: target_height,
            }),
        };
        if scissor.is_none() {
            return Ok(());
        }

        let texture_bind_group = self
            .texture_bind_group
            .as_ref()
            .expect("ensure_texture_bind_group initializes bind group");

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

        pass.set_pipeline(pipeline.as_ref());
        pass.set_bind_group(0, &self.constants_bind_group, &[]);
        pass.set_bind_group(1, texture_bind_group, &[]);
        pass.set_blend_constant(dynamic.blend_constant);
        pass.set_stencil_reference(dynamic.stencil_reference);
        pass.set_viewport(
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            viewport.min_depth,
            viewport.max_depth,
        );
        if let Some(rect) = scissor {
            pass.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
        }

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

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);

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

        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);

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

    fn ensure_texture_bind_group(&mut self) {
        if !self.texture_bind_group_dirty && self.texture_bind_group.is_some() {
            return;
        }

        let mut entries = Vec::with_capacity(MAX_SAMPLERS * 2);
        for slot in 0..MAX_SAMPLERS {
            let sampler = if self.bound_textures[slot].is_some() {
                &self.sampler_cache[slot]
            } else {
                &self.default_sampler
            };
            entries.push(wgpu::BindGroupEntry {
                binding: (slot * 2) as u32,
                resource: wgpu::BindingResource::Sampler(sampler),
            });

            let view = match self.bound_textures[slot] {
                Some(id) => self
                    .textures
                    .get(&id)
                    .map(|tex| &tex.view_mip0)
                    .unwrap_or(&self.default_texture_view),
                None => &self.default_texture_view,
            };
            entries.push(wgpu::BindGroupEntry {
                binding: (slot * 2 + 1) as u32,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }

        self.texture_bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d9-textures-bg"),
            layout: &self.texture_bind_group_layout,
            entries: &entries,
        }));
        self.texture_bind_group_dirty = false;
    }

    fn ensure_builtin_module(&mut self) -> Result<(), RuntimeError> {
        if self.builtin_shader_module.is_some() {
            return Ok(());
        }

        let wgsl = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

struct Constants {
    color: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> constants: Constants;

@group(1) @binding(0)
var samp0: sampler;

@group(1) @binding(1)
var tex0: texture_2d<f32>;

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.position = vec4<f32>(input.pos, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}

@fragment
fn fs_solid(_input: VsOut) -> @location(0) vec4<f32> {
    return constants.color;
}

@fragment
fn fs_textured(input: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex0, samp0, input.uv);
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

fn builtin_entry_points(key: &PipelineKey) -> Result<(&'static str, &'static str), RuntimeError> {
    let vs_entry = match key.vertex_shader.0 {
        1 => "vs_main",
        other => {
            return Err(RuntimeError::UnsupportedShaderKey(
                other.try_into().unwrap_or(u32::MAX),
            ))
        }
    };

    let fs_entry = match key.pixel_shader.0 {
        1 => "fs_solid",
        2 => "fs_textured",
        other => {
            return Err(RuntimeError::UnsupportedShaderKey(
                other.try_into().unwrap_or(u32::MAX),
            ))
        }
    };

    Ok((vs_entry, fs_entry))
}

fn create_render_pipeline(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    key: &PipelineKey,
    translated: &crate::state::TranslatedPipelineState,
    vs_entry: &'static str,
    fs_entry: &'static str,
) -> wgpu::RenderPipeline {
    let mut attribute_storage = Vec::with_capacity(key.vertex_layouts.len());
    for layout in &key.vertex_layouts {
        attribute_storage.push(
            layout
                .attributes
                .iter()
                .map(|attr| wgpu::VertexAttribute {
                    format: attr.format,
                    offset: attr.offset,
                    shader_location: attr.shader_location,
                })
                .collect::<Vec<_>>(),
        );
    }

    let vertex_buffers = key
        .vertex_layouts
        .iter()
        .zip(attribute_storage.iter())
        .map(|(layout, attrs)| wgpu::VertexBufferLayout {
            array_stride: layout.array_stride,
            step_mode: layout.step_mode,
            attributes: attrs.as_slice(),
        })
        .collect::<Vec<_>>();

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aero-d3d9-pipeline"),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: vs_entry,
            buffers: &vertex_buffers,
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: fs_entry,
            targets: &translated.targets,
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: translated.primitive,
        depth_stencil: translated.depth_stencil.clone(),
        multisample: translated.multisample,
        multiview: None,
    })
}

fn clamp_viewport(mut viewport: Viewport, target_width: u32, target_height: u32) -> Viewport {
    let tw = target_width as f32;
    let th = target_height as f32;

    viewport.min_depth = viewport.min_depth.clamp(0.0, 1.0);
    viewport.max_depth = viewport.max_depth.clamp(viewport.min_depth, 1.0);

    if viewport.width <= 0.0 || viewport.height <= 0.0 || tw == 0.0 || th == 0.0 {
        return Viewport {
            x: 0.0,
            y: 0.0,
            width: tw,
            height: th,
            min_depth: viewport.min_depth,
            max_depth: viewport.max_depth,
        };
    }

    viewport.x = viewport.x.clamp(0.0, tw);
    viewport.y = viewport.y.clamp(0.0, th);

    viewport.width = viewport.width.clamp(0.0, tw - viewport.x);
    viewport.height = viewport.height.clamp(0.0, th - viewport.y);

    if viewport.width <= 0.0 || viewport.height <= 0.0 {
        Viewport {
            x: 0.0,
            y: 0.0,
            width: tw,
            height: th,
            min_depth: viewport.min_depth,
            max_depth: viewport.max_depth,
        }
    } else {
        viewport
    }
}

fn clamp_scissor(rect: ScissorRect, target_width: u32, target_height: u32) -> Option<ScissorRect> {
    if rect.width == 0 || rect.height == 0 {
        return None;
    }
    if rect.x >= target_width || rect.y >= target_height {
        return None;
    }
    let max_w = target_width - rect.x;
    let max_h = target_height - rect.y;
    let width = rect.width.min(max_w);
    let height = rect.height.min(max_h);
    if width == 0 || height == 0 {
        None
    } else {
        Some(ScissorRect {
            x: rect.x,
            y: rect.y,
            width,
            height,
        })
    }
}

fn resolve_color_target_view<'a>(
    target: RenderTarget,
    wants_srgb: bool,
    swapchains: &'a HashMap<u32, SwapChainResource>,
    textures: &'a HashMap<u32, TextureResource>,
) -> Result<(&'a wgpu::TextureView, u32, u32), RuntimeError> {
    match target {
        RenderTarget::SwapChain(id) => {
            let swap = swapchains
                .get(&id)
                .ok_or(RuntimeError::UnknownSwapChain(id))?;
            let view = if wants_srgb {
                swap.view_srgb.as_ref().unwrap_or(&swap.view)
            } else {
                &swap.view
            };
            Ok((view, swap.desc.width, swap.desc.height))
        }
        RenderTarget::Texture(id) => {
            let tex = textures.get(&id).ok_or(RuntimeError::UnknownTexture(id))?;
            let view = if wants_srgb {
                tex.view_mip0_srgb.as_ref().unwrap_or(&tex.view_mip0)
            } else {
                &tex.view_mip0
            };
            Ok((view, tex.desc.width, tex.desc.height))
        }
    }
}

fn create_wgpu_sampler(
    device: &wgpu::Device,
    state: &crate::state::tracker::SamplerState,
    _fallback: &wgpu::Sampler,
) -> wgpu::Sampler {
    let _ = _fallback;

    fn addr(device: &wgpu::Device, value: u32) -> wgpu::AddressMode {
        match value {
            d3d9::D3DTADDRESS_WRAP | 0 => wgpu::AddressMode::Repeat,
            d3d9::D3DTADDRESS_MIRROR => wgpu::AddressMode::MirrorRepeat,
            d3d9::D3DTADDRESS_CLAMP => wgpu::AddressMode::ClampToEdge,
            d3d9::D3DTADDRESS_BORDER => {
                if device
                    .features()
                    .contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER)
                {
                    wgpu::AddressMode::ClampToBorder
                } else {
                    wgpu::AddressMode::ClampToEdge
                }
            }
            d3d9::D3DTADDRESS_MIRRORONCE => wgpu::AddressMode::MirrorRepeat,
            _ => wgpu::AddressMode::Repeat,
        }
    }

    fn filter(value: u32) -> wgpu::FilterMode {
        match value {
            d3d9::D3DTEXF_POINT => wgpu::FilterMode::Nearest,
            d3d9::D3DTEXF_LINEAR | d3d9::D3DTEXF_ANISOTROPIC => wgpu::FilterMode::Linear,
            d3d9::D3DTEXF_NONE => wgpu::FilterMode::Linear,
            _ => wgpu::FilterMode::Linear,
        }
    }

    let address_mode_u = addr(device, state.address_u);
    let address_mode_v = addr(device, state.address_v);
    let address_mode_w = wgpu::AddressMode::ClampToEdge;

    let min_filter = filter(state.min_filter);
    let mag_filter = filter(state.mag_filter);
    let mipmap_filter = filter(state.mip_filter);
    let lod_max_clamp = if state.mip_filter == d3d9::D3DTEXF_NONE || state.mip_filter == 0 {
        0.0
    } else {
        32.0
    };

    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("aero-d3d9-sampler"),
        address_mode_u,
        address_mode_v,
        address_mode_w,
        mag_filter,
        min_filter,
        mipmap_filter,
        lod_min_clamp: 0.0,
        lod_max_clamp,
        compare: None,
        anisotropy_clamp: 1,
        border_color: None,
    })
}

mod d3d9 {
    // D3DRENDERSTATETYPE (subset).
    pub const D3DRS_ZENABLE: u32 = 7;
    pub const D3DRS_ZWRITEENABLE: u32 = 14;
    pub const D3DRS_ZFUNC: u32 = 23;
    pub const D3DRS_CULLMODE: u32 = 22;
    pub const D3DRS_FRONTCOUNTERCLOCKWISE: u32 = 18;

    pub const D3DRS_ALPHABLENDENABLE: u32 = 27;
    pub const D3DRS_SRCBLEND: u32 = 19;
    pub const D3DRS_DESTBLEND: u32 = 20;
    pub const D3DRS_BLENDOP: u32 = 171;
    pub const D3DRS_SEPARATEALPHABLENDENABLE: u32 = 206;
    pub const D3DRS_SRCBLENDALPHA: u32 = 207;
    pub const D3DRS_DESTBLENDALPHA: u32 = 208;
    pub const D3DRS_BLENDOPALPHA: u32 = 209;
    pub const D3DRS_BLENDFACTOR: u32 = 193;

    pub const D3DRS_COLORWRITEENABLE: u32 = 168;
    pub const D3DRS_COLORWRITEENABLE1: u32 = 190;
    pub const D3DRS_COLORWRITEENABLE2: u32 = 191;
    pub const D3DRS_COLORWRITEENABLE3: u32 = 192;

    pub const D3DRS_SRGBWRITEENABLE: u32 = 194;
    pub const D3DRS_SCISSORTESTENABLE: u32 = 174;

    pub const D3DRS_STENCILENABLE: u32 = 52;
    pub const D3DRS_STENCILFAIL: u32 = 53;
    pub const D3DRS_STENCILZFAIL: u32 = 54;
    pub const D3DRS_STENCILPASS: u32 = 55;
    pub const D3DRS_STENCILFUNC: u32 = 56;
    pub const D3DRS_STENCILREF: u32 = 57;
    pub const D3DRS_STENCILMASK: u32 = 58;
    pub const D3DRS_STENCILWRITEMASK: u32 = 59;

    // D3DSAMPLERSTATETYPE (subset).
    pub const D3DSAMP_ADDRESSU: u32 = 1;
    pub const D3DSAMP_ADDRESSV: u32 = 2;
    pub const D3DSAMP_MAGFILTER: u32 = 5;
    pub const D3DSAMP_MINFILTER: u32 = 6;
    pub const D3DSAMP_MIPFILTER: u32 = 7;

    // D3DTEXTUREADDRESS.
    pub const D3DTADDRESS_WRAP: u32 = 1;
    pub const D3DTADDRESS_MIRROR: u32 = 2;
    pub const D3DTADDRESS_CLAMP: u32 = 3;
    pub const D3DTADDRESS_BORDER: u32 = 4;
    pub const D3DTADDRESS_MIRRORONCE: u32 = 5;

    // D3DTEXTUREFILTERTYPE (subset).
    pub const D3DTEXF_NONE: u32 = 0;
    pub const D3DTEXF_POINT: u32 = 1;
    pub const D3DTEXF_LINEAR: u32 = 2;
    pub const D3DTEXF_ANISOTROPIC: u32 = 3;

    // Blend factors.
    pub const D3DBLEND_ZERO: u32 = 1;
    pub const D3DBLEND_ONE: u32 = 2;
    pub const D3DBLEND_SRCCOLOR: u32 = 3;
    pub const D3DBLEND_INVSRCCOLOR: u32 = 4;
    pub const D3DBLEND_SRCALPHA: u32 = 5;
    pub const D3DBLEND_INVSRCALPHA: u32 = 6;
    pub const D3DBLEND_DESTALPHA: u32 = 7;
    pub const D3DBLEND_INVDESTALPHA: u32 = 8;
    pub const D3DBLEND_DESTCOLOR: u32 = 9;
    pub const D3DBLEND_INVDESTCOLOR: u32 = 10;
    pub const D3DBLEND_SRCALPHASAT: u32 = 11;
    pub const D3DBLEND_BOTHSRCALPHA: u32 = 12;
    pub const D3DBLEND_BOTHINVSRCALPHA: u32 = 13;
    pub const D3DBLEND_BLENDFACTOR: u32 = 14;
    pub const D3DBLEND_INVBLENDFACTOR: u32 = 15;

    // Blend ops.
    pub const D3DBLENDOP_ADD: u32 = 1;
    pub const D3DBLENDOP_SUBTRACT: u32 = 2;
    pub const D3DBLENDOP_REVSUBTRACT: u32 = 3;
    pub const D3DBLENDOP_MIN: u32 = 4;
    pub const D3DBLENDOP_MAX: u32 = 5;

    // Compare funcs.
    pub const D3DCMP_NEVER: u32 = 1;
    pub const D3DCMP_LESS: u32 = 2;
    pub const D3DCMP_EQUAL: u32 = 3;
    pub const D3DCMP_LESSEQUAL: u32 = 4;
    pub const D3DCMP_GREATER: u32 = 5;
    pub const D3DCMP_NOTEQUAL: u32 = 6;
    pub const D3DCMP_GREATEREQUAL: u32 = 7;
    pub const D3DCMP_ALWAYS: u32 = 8;

    // Stencil ops.
    pub const D3DSTENCILOP_KEEP: u32 = 1;
    pub const D3DSTENCILOP_ZERO: u32 = 2;
    pub const D3DSTENCILOP_REPLACE: u32 = 3;
    pub const D3DSTENCILOP_INCRSAT: u32 = 4;
    pub const D3DSTENCILOP_DECRSAT: u32 = 5;
    pub const D3DSTENCILOP_INVERT: u32 = 6;
    pub const D3DSTENCILOP_INCR: u32 = 7;
    pub const D3DSTENCILOP_DECR: u32 = 8;

    // Cull modes.
    pub const D3DCULL_NONE: u32 = 1;
    pub const D3DCULL_CW: u32 = 2;
    pub const D3DCULL_CCW: u32 = 3;
}

fn d3d9_compare_func(value: u32) -> Option<CompareFunc> {
    Some(match value {
        d3d9::D3DCMP_NEVER => CompareFunc::Never,
        d3d9::D3DCMP_LESS => CompareFunc::Less,
        d3d9::D3DCMP_EQUAL => CompareFunc::Equal,
        d3d9::D3DCMP_LESSEQUAL => CompareFunc::LessEqual,
        d3d9::D3DCMP_GREATER => CompareFunc::Greater,
        d3d9::D3DCMP_NOTEQUAL => CompareFunc::NotEqual,
        d3d9::D3DCMP_GREATEREQUAL => CompareFunc::GreaterEqual,
        d3d9::D3DCMP_ALWAYS => CompareFunc::Always,
        _ => return None,
    })
}

fn d3d9_stencil_op(value: u32) -> Option<StencilOp> {
    Some(match value {
        d3d9::D3DSTENCILOP_KEEP => StencilOp::Keep,
        d3d9::D3DSTENCILOP_ZERO => StencilOp::Zero,
        d3d9::D3DSTENCILOP_REPLACE => StencilOp::Replace,
        d3d9::D3DSTENCILOP_INCRSAT => StencilOp::IncrSat,
        d3d9::D3DSTENCILOP_DECRSAT => StencilOp::DecrSat,
        d3d9::D3DSTENCILOP_INVERT => StencilOp::Invert,
        d3d9::D3DSTENCILOP_INCR => StencilOp::Incr,
        d3d9::D3DSTENCILOP_DECR => StencilOp::Decr,
        _ => return None,
    })
}

fn d3d9_blend_factor(value: u32) -> Option<BlendFactor> {
    Some(match value {
        d3d9::D3DBLEND_ZERO => BlendFactor::Zero,
        d3d9::D3DBLEND_ONE => BlendFactor::One,
        d3d9::D3DBLEND_SRCCOLOR => BlendFactor::SrcColor,
        d3d9::D3DBLEND_INVSRCCOLOR => BlendFactor::InvSrcColor,
        d3d9::D3DBLEND_SRCALPHA => BlendFactor::SrcAlpha,
        d3d9::D3DBLEND_INVSRCALPHA => BlendFactor::InvSrcAlpha,
        d3d9::D3DBLEND_DESTALPHA => BlendFactor::DestAlpha,
        d3d9::D3DBLEND_INVDESTALPHA => BlendFactor::InvDestAlpha,
        d3d9::D3DBLEND_DESTCOLOR => BlendFactor::DestColor,
        d3d9::D3DBLEND_INVDESTCOLOR => BlendFactor::InvDestColor,
        d3d9::D3DBLEND_SRCALPHASAT => BlendFactor::SrcAlphaSat,
        d3d9::D3DBLEND_BLENDFACTOR => BlendFactor::BlendFactor,
        d3d9::D3DBLEND_INVBLENDFACTOR => BlendFactor::InvBlendFactor,
        _ => return None,
    })
}

fn d3d9_blend_op(value: u32) -> Option<BlendOp> {
    Some(match value {
        d3d9::D3DBLENDOP_ADD => BlendOp::Add,
        d3d9::D3DBLENDOP_SUBTRACT => BlendOp::Subtract,
        d3d9::D3DBLENDOP_REVSUBTRACT => BlendOp::RevSubtract,
        d3d9::D3DBLENDOP_MIN => BlendOp::Min,
        d3d9::D3DBLENDOP_MAX => BlendOp::Max,
        _ => return None,
    })
}

fn d3d9_cull_mode(value: u32) -> Option<CullMode> {
    Some(match value {
        d3d9::D3DCULL_NONE => CullMode::None,
        d3d9::D3DCULL_CW => CullMode::CW,
        d3d9::D3DCULL_CCW => CullMode::CCW,
        _ => return None,
    })
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
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);

    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);
    let _ = receiver.receive().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiated_features_respects_texture_compression_opt_out() {
        let compression = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ETC2
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR;

        let available = compression;

        let requested = negotiated_features_for_available(available, false, false);
        assert!(requested.contains(compression));

        let requested = negotiated_features_for_available(available, false, true);
        assert!(!requested.intersects(compression));
    }

    #[test]
    fn negotiated_features_only_requests_adapter_supported_bits() {
        let requested = negotiated_features_for_available(wgpu::Features::empty(), false, false);
        assert!(requested.is_empty());
    }

    #[test]
    fn negotiated_features_only_requests_supported_compression_features() {
        let available = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let requested = negotiated_features_for_available(available, false, false);
        assert!(requested.contains(wgpu::Features::TEXTURE_COMPRESSION_BC));
        assert!(!requested.contains(wgpu::Features::TEXTURE_COMPRESSION_ETC2));
        assert!(!requested.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR));
    }

    #[test]
    fn negotiated_features_disables_compression_on_gl_backend() {
        let compression = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ETC2
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR;

        let requested = negotiated_features_for_available(compression, true, false);
        assert!(
            !requested.intersects(compression),
            "compression features must not be requested on the wgpu GL backend"
        );
    }
}
