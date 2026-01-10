pub mod handles;
pub mod registry;

pub use handles::*;
pub use registry::*;
use crate::{GpuCapabilities, GpuError};

/// Which backend implementation is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// WebGPU via `wgpu` (native backends or browser WebGPU).
    WebGpu,
    /// WebGL2 via `wgpu`'s GL emulation layer.
    WebGl2Wgpu,
    /// Raw WebGL2 presenter (CPU RGBA8 blit) – no general GPU functionality.
    WebGl2Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferUsages(u32);

impl BufferUsages {
    pub const MAP_READ: Self = Self(1 << 0);
    pub const MAP_WRITE: Self = Self(1 << 1);
    pub const COPY_SRC: Self = Self(1 << 2);
    pub const COPY_DST: Self = Self(1 << 3);
    pub const INDEX: Self = Self(1 << 4);
    pub const VERTEX: Self = Self(1 << 5);
    pub const UNIFORM: Self = Self(1 << 6);
    pub const STORAGE: Self = Self(1 << 7);
    pub const INDIRECT: Self = Self(1 << 8);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for BufferUsages {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for BufferUsages {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureUsages(u32);

impl TextureUsages {
    pub const COPY_SRC: Self = Self(1 << 0);
    pub const COPY_DST: Self = Self(1 << 1);
    pub const TEXTURE_BINDING: Self = Self(1 << 2);
    pub const STORAGE_BINDING: Self = Self(1 << 3);
    pub const RENDER_ATTACHMENT: Self = Self(1 << 4);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for TextureUsages {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for TextureUsages {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderStages(u32);

impl ShaderStages {
    pub const VERTEX: Self = Self(1 << 0);
    pub const FRAGMENT: Self = Self(1 << 1);
    pub const COMPUTE: Self = Self(1 << 2);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for ShaderStages {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for ShaderStages {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone)]
pub struct BufferDesc {
    pub label: Option<String>,
    pub size: u64,
    pub usage: BufferUsages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    Depth24Plus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureDimension {
    D2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extent3d {
    pub width: u32,
    pub height: u32,
    pub depth_or_array_layers: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Origin3d {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

impl Origin3d {
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDataLayout {
    pub offset: u64,
    pub bytes_per_row: Option<u32>,
    pub rows_per_image: Option<u32>,
}

impl Default for ImageDataLayout {
    fn default() -> Self {
        Self {
            offset: 0,
            bytes_per_row: None,
            rows_per_image: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextureDesc {
    pub label: Option<String>,
    pub size: Extent3d,
    pub mip_level_count: u32,
    pub sample_count: u32,
    pub dimension: TextureDimension,
    pub format: TextureFormat,
    pub usage: TextureUsages,
}

#[derive(Debug, Clone, Default)]
pub struct TextureViewDesc {}

#[derive(Debug, Clone)]
pub struct TextureWriteDesc {
    pub texture: TextureId,
    pub mip_level: u32,
    pub origin: Origin3d,
    pub layout: ImageDataLayout,
    pub size: Extent3d,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    Nearest,
    Linear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressMode {
    ClampToEdge,
    Repeat,
    MirrorRepeat,
}

#[derive(Debug, Clone)]
pub struct SamplerDesc {
    pub label: Option<String>,
    pub address_mode_u: AddressMode,
    pub address_mode_v: AddressMode,
    pub address_mode_w: AddressMode,
    pub mag_filter: FilterMode,
    pub min_filter: FilterMode,
    pub mipmap_filter: FilterMode,
}

impl Default for SamplerDesc {
    fn default() -> Self {
        Self {
            label: None,
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Nearest,
            mipmap_filter: FilterMode::Nearest,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BindGroupLayoutDesc {
    pub label: Option<String>,
    pub entries: Vec<BindGroupLayoutEntryDesc>,
}

#[derive(Debug, Clone)]
pub struct BindGroupLayoutEntryDesc {
    pub binding: u32,
    pub visibility: ShaderStages,
    pub ty: BindingTypeDesc,
}

#[derive(Debug, Clone)]
pub enum BindingTypeDesc {
    UniformBuffer {
        dynamic: bool,
        min_size: Option<u64>,
    },
    SamplerFiltering,
    Texture2dFloat {
        filterable: bool,
    },
}

#[derive(Debug, Clone)]
pub struct BindGroupDesc {
    pub label: Option<String>,
    pub layout: BindGroupLayoutId,
    pub entries: Vec<BindGroupEntryDesc>,
}

#[derive(Debug, Clone)]
pub struct BindGroupEntryDesc {
    pub binding: u32,
    pub resource: BindingResourceDesc,
}

#[derive(Debug, Clone)]
pub enum BindingResourceDesc {
    Buffer {
        buffer: BufferId,
        offset: u64,
        size: Option<u64>,
    },
    Sampler(SamplerId),
    TextureView(TextureViewId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveTopology {
    TriangleList,
}

#[derive(Debug, Clone)]
pub struct RenderPipelineDesc {
    pub label: Option<String>,
    pub shader_wgsl: String,
    pub vertex_entry: String,
    pub fragment_entry: String,
    pub bind_group_layouts: Vec<BindGroupLayoutId>,
    pub color_format: TextureFormat,
    pub depth_format: Option<TextureFormat>,
    pub topology: PrimitiveTopology,
}

#[derive(Debug, Clone)]
pub struct ComputePipelineDesc {
    pub label: Option<String>,
    pub shader_wgsl: String,
    pub entry_point: String,
    pub bind_group_layouts: Vec<BindGroupLayoutId>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadOp<T> {
    Load,
    Clear(T),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOp {
    Store,
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Operations<T> {
    pub load: LoadOp<T>,
    pub store: StoreOp,
}

#[derive(Debug, Clone)]
pub struct RenderPassColorAttachmentDesc {
    pub view: TextureViewId,
    pub ops: Operations<Color>,
}

#[derive(Debug, Clone)]
pub struct RenderPassDesc {
    pub label: Option<String>,
    pub color_attachments: Vec<RenderPassColorAttachmentDesc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexFormat {
    Uint16,
    Uint32,
}

#[derive(Debug, Clone)]
pub enum GpuCommand {
    BeginRenderPass(RenderPassDesc),
    EndRenderPass,
    BeginComputePass {
        label: Option<String>,
    },
    EndComputePass,
    SetPipeline(PipelineId),
    SetBindGroup {
        index: u32,
        bind_group: BindGroupId,
    },
    SetVertexBuffer {
        slot: u32,
        buffer: BufferId,
        offset: u64,
    },
    SetIndexBuffer {
        buffer: BufferId,
        offset: u64,
        format: IndexFormat,
    },
    Draw {
        vertices: core::ops::Range<u32>,
        instances: core::ops::Range<u32>,
    },
    DrawIndexed {
        indices: core::ops::Range<u32>,
        base_vertex: i32,
        instances: core::ops::Range<u32>,
    },
    DispatchWorkgroups {
        x: u32,
        y: u32,
        z: u32,
    },
}

/// Backend-agnostic GPU interface used by higher-level code.
///
/// All resources are created and referred to via opaque handles (e.g. `BufferId`). Backends must
/// validate generation counters and return structured errors rather than panicking.
pub trait GpuBackend {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> &GpuCapabilities;

    fn create_buffer(&mut self, desc: BufferDesc) -> Result<BufferId, GpuError>;
    fn destroy_buffer(&mut self, id: BufferId) -> Result<(), GpuError>;
    fn write_buffer(&mut self, buffer: BufferId, offset: u64, data: &[u8]) -> Result<(), GpuError>;

    fn create_texture(&mut self, desc: TextureDesc) -> Result<TextureId, GpuError>;
    fn destroy_texture(&mut self, id: TextureId) -> Result<(), GpuError>;
    fn write_texture(&mut self, _desc: TextureWriteDesc, _data: &[u8]) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("write_texture"))
    }

    fn create_texture_view(
        &mut self,
        texture: TextureId,
        desc: TextureViewDesc,
    ) -> Result<TextureViewId, GpuError>;
    fn destroy_texture_view(&mut self, id: TextureViewId) -> Result<(), GpuError>;

    fn create_sampler(&mut self, desc: SamplerDesc) -> Result<SamplerId, GpuError>;
    fn destroy_sampler(&mut self, id: SamplerId) -> Result<(), GpuError>;

    fn create_bind_group_layout(
        &mut self,
        desc: BindGroupLayoutDesc,
    ) -> Result<BindGroupLayoutId, GpuError>;
    fn destroy_bind_group_layout(&mut self, id: BindGroupLayoutId) -> Result<(), GpuError>;

    fn create_bind_group(&mut self, desc: BindGroupDesc) -> Result<BindGroupId, GpuError>;
    fn destroy_bind_group(&mut self, id: BindGroupId) -> Result<(), GpuError>;

    fn create_render_pipeline(&mut self, desc: RenderPipelineDesc) -> Result<PipelineId, GpuError>;
    fn create_compute_pipeline(
        &mut self,
        desc: ComputePipelineDesc,
    ) -> Result<PipelineId, GpuError>;
    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<(), GpuError>;

    fn create_command_buffer(
        &mut self,
        commands: &[GpuCommand],
    ) -> Result<CommandBufferId, GpuError>;
    fn submit(&mut self, command_buffers: &[CommandBufferId]) -> Result<(), GpuError>;

    fn present(&mut self) -> Result<(), GpuError>;

    fn present_rgba8_framebuffer(
        &mut self,
        _width: u32,
        _height: u32,
        _rgba8: &[u8],
    ) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("present_rgba8_framebuffer"))
    }

    fn screenshot_rgba8(&mut self) -> Result<Vec<u8>, GpuError> {
        Err(GpuError::Unsupported("screenshot"))
    }
}
