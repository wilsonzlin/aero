//! Host-side executor for the stable AeroGPU guest↔host command stream.
//!
//! The "real" executor is expected to translate the full AeroGPU IR into WebGPU
//! commands. For now we implement a minimal subset needed for validating
//! guest-memory-backed resources (`alloc_table` + `backing_alloc_id`) and
//! `RESOURCE_DIRTY_RANGE` uploads.

use std::collections::HashMap;
use std::ops::Range;

use futures_intrusive::channel::shared::oneshot_channel;

use crate::guest_memory::{GuestMemory, GuestMemoryError};
use crate::{
    decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8,
    TextureUploadTransform,
};

use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_pci as pci, aerogpu_ring as ring};

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, ExecutorError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(ExecutorError::TruncatedPacket)?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn align_down_u64(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    value & !(alignment - 1)
}

fn align_up_u64(value: u64, alignment: u64) -> Result<u64, ExecutorError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or_else(|| ExecutorError::Validation("alignment overflow".into()))
}

fn align_up_u32(value: u32, alignment: u32) -> Result<u32, ExecutorError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or_else(|| ExecutorError::Validation("alignment overflow".into()))
}

fn is_x8_format(format_raw: u32) -> bool {
    format_raw == pci::AerogpuFormat::B8G8R8X8Unorm as u32
        || format_raw == pci::AerogpuFormat::R8G8B8X8Unorm as u32
        || format_raw == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
        || format_raw == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32
}

fn is_bc_format(format_raw: u32) -> bool {
    format_raw == pci::AerogpuFormat::BC1RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC1RgbaUnormSrgb as u32
        || format_raw == pci::AerogpuFormat::BC2RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC2RgbaUnormSrgb as u32
        || format_raw == pci::AerogpuFormat::BC3RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC3RgbaUnormSrgb as u32
        || format_raw == pci::AerogpuFormat::BC7RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC7RgbaUnormSrgb as u32
}

#[derive(Debug, Clone, Copy)]
struct TextureCopyLayout {
    block_w: u32,
    block_h: u32,
    block_bytes: u32,

    /// Number of rows in `ImageDataLayout` / guest memory layout (texel rows for uncompressed,
    /// block rows for BC).
    rows_in_layout: u32,
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
}

fn texture_copy_layout(
    width: u32,
    height: u32,
    format_raw: u32,
) -> Result<TextureCopyLayout, ExecutorError> {
    let (block_w, block_h, block_bytes) = match format_raw {
        v if v == pci::AerogpuFormat::B8G8R8A8Unorm as u32
            || v == pci::AerogpuFormat::B8G8R8X8Unorm as u32
            || v == pci::AerogpuFormat::R8G8B8A8Unorm as u32
            || v == pci::AerogpuFormat::R8G8B8X8Unorm as u32
            || v == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
            || v == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
            || v == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32
            || v == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
        {
            (1, 1, 4)
        }
        v if v == pci::AerogpuFormat::BC1RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC1RgbaUnormSrgb as u32 =>
        {
            (4, 4, 8)
        }
        v if v == pci::AerogpuFormat::BC2RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC2RgbaUnormSrgb as u32
            || v == pci::AerogpuFormat::BC3RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC3RgbaUnormSrgb as u32
            || v == pci::AerogpuFormat::BC7RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC7RgbaUnormSrgb as u32 =>
        {
            (4, 4, 16)
        }
        _ => {
            return Err(ExecutorError::Validation(format!(
                "unsupported aerogpu_format={format_raw}"
            )))
        }
    };

    let blocks_w = width.div_ceil(block_w);
    let blocks_h = height.div_ceil(block_h);
    let unpadded_bytes_per_row = blocks_w
        .checked_mul(block_bytes)
        .ok_or_else(|| ExecutorError::Validation("texture bytes_per_row overflow".into()))?;
    let padded_bytes_per_row =
        align_up_u32(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)?;
    Ok(TextureCopyLayout {
        block_w,
        block_h,
        block_bytes,
        rows_in_layout: blocks_h,
        unpadded_bytes_per_row,
        padded_bytes_per_row,
    })
}

fn force_opaque_alpha_rgba8(pixels: &mut [u8]) {
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 0xFF;
    }
}

fn map_cmd_stream_parse_error(err: AeroGpuCmdStreamParseError) -> ExecutorError {
    match err {
        AeroGpuCmdStreamParseError::BufferTooSmall => ExecutorError::TruncatedStream,
        AeroGpuCmdStreamParseError::InvalidMagic(found) => ExecutorError::BadStreamMagic(found),
        AeroGpuCmdStreamParseError::UnsupportedAbiMajor { found } => {
            ExecutorError::Validation(format!("unsupported ABI major version {found}"))
        }
        AeroGpuCmdStreamParseError::InvalidSizeBytes {
            size_bytes,
            buffer_len,
        } => ExecutorError::BadStreamSize {
            size_bytes,
            buffer_len,
        },
        AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(found) => {
            ExecutorError::InvalidPacketSize(found)
        }
        AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(found) => {
            ExecutorError::MisalignedPacketSize(found)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("command stream too small")]
    TruncatedStream,
    #[error("invalid command stream magic 0x{0:08x}")]
    BadStreamMagic(u32),
    #[error("invalid command stream size_bytes={size_bytes} (buffer_len={buffer_len})")]
    BadStreamSize { size_bytes: u32, buffer_len: usize },
    #[error("truncated packet")]
    TruncatedPacket,
    #[error("invalid packet size_bytes={0}")]
    InvalidPacketSize(u32),
    #[error("packet size_bytes={0} is not 4-byte aligned")]
    MisalignedPacketSize(u32),

    #[error("validation error: {0}")]
    Validation(String),

    #[error(transparent)]
    GuestMemory(#[from] GuestMemoryError),
}

#[derive(Debug, Clone)]
pub enum ExecutorEvent {
    Error { at: usize, message: String },
}

#[derive(Debug, Clone)]
pub struct ExecutionReport {
    pub packets_processed: u32,
    pub events: Vec<ExecutorEvent>,
}

impl ExecutionReport {
    pub fn is_ok(&self) -> bool {
        !self
            .events
            .iter()
            .any(|e| matches!(e, ExecutorEvent::Error { .. }))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AllocEntry {
    pub flags: u32,
    pub gpa: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Default, Clone)]
pub struct AllocTable {
    entries: HashMap<u32, AllocEntry>,
}

impl AllocTable {
    pub fn new(
        entries: impl IntoIterator<Item = (u32, AllocEntry)>,
    ) -> Result<Self, ExecutorError> {
        let mut map = HashMap::<u32, AllocEntry>::new();
        for (alloc_id, entry) in entries {
            if alloc_id == 0 {
                return Err(ExecutorError::Validation(
                    "alloc table entry alloc_id must be non-zero".into(),
                ));
            }
            if entry.size_bytes == 0 {
                return Err(ExecutorError::Validation(format!(
                    "alloc table entry {alloc_id} has size_bytes=0"
                )));
            }
            if entry.gpa.checked_add(entry.size_bytes).is_none() {
                return Err(ExecutorError::Validation(format!(
                    "alloc table entry {alloc_id} gpa+size overflow"
                )));
            }
            if let Some(existing) = map.insert(alloc_id, entry) {
                return Err(ExecutorError::Validation(format!(
                    "alloc table contains duplicate alloc_id={alloc_id} (gpa0=0x{:x} size0={} gpa1=0x{:x} size1={})",
                    existing.gpa, existing.size_bytes, entry.gpa, entry.size_bytes
                )));
            }
        }
        Ok(Self { entries: map })
    }

    pub fn get(&self, alloc_id: u32) -> Option<&AllocEntry> {
        self.entries.get(&alloc_id)
    }

    fn resolve_gpa(&self, alloc_id: u32, offset: u64, size: u64) -> Result<u64, ExecutorError> {
        let entry = self.get(alloc_id).ok_or_else(|| {
            ExecutorError::Validation(format!("missing alloc table entry for alloc_id={alloc_id}"))
        })?;

        let end = offset.checked_add(size).ok_or_else(|| {
            ExecutorError::Validation("alloc table range offset+size overflow".into())
        })?;
        if end > entry.size_bytes {
            return Err(ExecutorError::Validation(format!(
                "alloc table range out of bounds for alloc_id={alloc_id} (offset=0x{offset:x}, size=0x{size:x}, alloc_size=0x{:x})",
                entry.size_bytes
            )));
        }

        let gpa = entry
            .gpa
            .checked_add(offset)
            .ok_or_else(|| ExecutorError::Validation("alloc table gpa+offset overflow".into()))?;
        if gpa.checked_add(size).is_none() {
            return Err(ExecutorError::Validation(
                "alloc table gpa+size overflow".into(),
            ));
        }

        Ok(gpa)
    }

    pub fn decode_from_guest_memory(
        guest_memory: &mut dyn GuestMemory,
        table_gpa: u64,
        table_size_bytes: u32,
    ) -> Result<Self, ExecutorError> {
        const MAX_ALLOC_TABLE_SIZE_BYTES: u32 = 16 * 1024 * 1024;

        if table_gpa == 0 || table_size_bytes == 0 {
            return Err(ExecutorError::Validation(
                "alloc table gpa/size must be non-zero".into(),
            ));
        }
        if table_gpa.checked_add(u64::from(table_size_bytes)).is_none() {
            return Err(ExecutorError::Validation(
                "alloc table gpa+size overflow".into(),
            ));
        }

        let table_size = table_size_bytes as usize;
        if table_size < ring::AerogpuAllocTableHeader::SIZE_BYTES {
            return Err(ExecutorError::Validation(format!(
                "alloc table size_bytes too small (got {table_size_bytes}, need {})",
                ring::AerogpuAllocTableHeader::SIZE_BYTES
            )));
        }

        let mut header_bytes = [0u8; ring::AerogpuAllocTableHeader::SIZE_BYTES];
        guest_memory.read(table_gpa, &mut header_bytes)?;
        let header =
            ring::AerogpuAllocTableHeader::decode_from_le_bytes(&header_bytes).map_err(|err| {
                ExecutorError::Validation(format!("failed to decode alloc table header: {err:?}"))
            })?;
        header.validate_prefix().map_err(|err| {
            ExecutorError::Validation(format!("invalid alloc table header: {err:?}"))
        })?;

        let size_bytes = header.size_bytes;
        if size_bytes > MAX_ALLOC_TABLE_SIZE_BYTES {
            return Err(ExecutorError::Validation(format!(
                "alloc table header size_bytes too large (got {size_bytes}, max {MAX_ALLOC_TABLE_SIZE_BYTES})"
            )));
        }
        let size_usize = size_bytes as usize;
        if size_usize < ring::AerogpuAllocTableHeader::SIZE_BYTES || size_usize > table_size {
            return Err(ExecutorError::Validation(format!(
                "invalid alloc table header size_bytes={size_bytes} (provided buffer size={table_size_bytes})"
            )));
        }
        // Forward-compat: newer guests may extend `aerogpu_alloc_entry` and increase the declared
        // stride; we only read the entry prefix we understand.
        if header.entry_stride_bytes < ring::AerogpuAllocEntry::SIZE_BYTES as u32 {
            return Err(ExecutorError::Validation(format!(
                "invalid alloc table entry_stride_bytes={} (expected at least {})",
                header.entry_stride_bytes,
                ring::AerogpuAllocEntry::SIZE_BYTES
            )));
        }

        let entry_count = header.entry_count;
        let entry_stride_bytes = header.entry_stride_bytes;

        let entry_count_usize = usize::try_from(entry_count).map_err(|_| {
            ExecutorError::Validation(
                "alloc table entry_count is out of range for host usize".into(),
            )
        })?;
        let mut entries = Vec::<(u32, AllocEntry)>::new();
        if entries.try_reserve_exact(entry_count_usize).is_err() {
            return Err(ExecutorError::Validation(format!(
                "alloc table too large to allocate (entry_count={entry_count})"
            )));
        }
        for i in 0..entry_count {
            let entry_offset = (i as u64)
                .checked_mul(entry_stride_bytes as u64)
                .ok_or_else(|| {
                    ExecutorError::Validation("alloc table entry offset overflow".into())
                })?;
            let entry_gpa = table_gpa
                .checked_add(ring::AerogpuAllocTableHeader::SIZE_BYTES as u64)
                .and_then(|gpa| gpa.checked_add(entry_offset))
                .ok_or_else(|| {
                    ExecutorError::Validation("alloc table entry gpa overflow".into())
                })?;
            let mut entry_bytes = [0u8; ring::AerogpuAllocEntry::SIZE_BYTES];
            guest_memory.read(entry_gpa, &mut entry_bytes)?;

            let entry =
                ring::AerogpuAllocEntry::decode_from_le_bytes(&entry_bytes).map_err(|err| {
                    ExecutorError::Validation(format!(
                        "failed to decode alloc table entry {i}: {err:?}"
                    ))
                })?;
            entries.push((
                entry.alloc_id,
                AllocEntry {
                    flags: entry.flags,
                    gpa: entry.gpa,
                    size_bytes: entry.size_bytes,
                },
            ));
        }

        AllocTable::new(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct GuestBufferBacking {
    alloc_id: u32,
    alloc_offset_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct GuestTextureBacking {
    alloc_id: u32,
    alloc_offset_bytes: u64,
    row_pitch_bytes: u32,
    size_bytes: u64,
}

#[derive(Debug)]
struct BufferResource {
    buffer: wgpu::Buffer,
    size_bytes: u64,
    usage_flags: u32,
    backing: Option<GuestBufferBacking>,
    dirty_ranges: Vec<Range<u64>>,
}

#[derive(Debug)]
struct TextureResource {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    usage_flags: u32,
    format_raw: u32,
    row_pitch_bytes: u32,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    upload_transform: TextureUploadTransform,
    /// The row pitch for `UPLOAD_RESOURCE`/dirty uploads, expressed in "layout rows":
    /// - For uncompressed RGBA/BGRA formats: texel rows.
    /// - For BC formats: 4x4 block rows.
    layout_row_pitch_bytes: u32,
    backing: Option<GuestTextureBacking>,
    dirty_ranges: Vec<Range<u64>>,
}

#[derive(Debug, Clone, Copy)]
struct CreateTexture2dArgs {
    texture_handle: u32,
    usage_flags: u32,
    format: u32,
    width: u32,
    height: u32,
    mip_levels: u32,
    array_layers: u32,
    row_pitch_bytes: u32,
    backing_alloc_id: u32,
    backing_offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct BoundVertexBuffer {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct BoundIndexBuffer {
    buffer: u32,
    format: wgpu::IndexFormat,
    offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct CopyBufferArgs {
    dst_buffer: u32,
    src_buffer: u32,
    dst_offset_bytes: u64,
    src_offset_bytes: u64,
    size_bytes: u64,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
struct CopyTexture2dArgs {
    dst_texture: u32,
    src_texture: u32,
    dst_mip_level: u32,
    dst_array_layer: u32,
    src_mip_level: u32,
    src_array_layer: u32,
    dst_x: u32,
    dst_y: u32,
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
struct DrawIndexedArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

#[derive(Debug, Default)]
struct ExecutorState {
    render_target: Option<u32>,
    vertex_buffer: Option<BoundVertexBuffer>,
    index_buffer: Option<BoundIndexBuffer>,
    pixel_texture0: Option<u32>,
}

/// Minimal host-side executor that implements the resource backing + dirty-range upload logic.
///
/// This is currently test-focused and only implements a subset of the full AeroGPU IR.
pub struct AeroGpuExecutor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    buffers: HashMap<u32, BufferResource>,
    textures: HashMap<u32, TextureResource>,

    state: ExecutorState,

    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

#[derive(Debug, Clone, Copy)]
struct TextureWritebackPlan {
    base_gpa: u64,
    row_pitch: u64,
    rows: u32,
    is_x8: bool,
    padded_bytes_per_row: u32,
    unpadded_bytes_per_row: u32,
}

#[derive(Debug)]
enum PendingWriteback {
    Buffer {
        staging: wgpu::Buffer,
        dst_gpa: u64,
        size_bytes: u64,
    },
    Texture2d {
        staging: wgpu::Buffer,
        plan: TextureWritebackPlan,
    },
}

impl AeroGpuExecutor {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, ExecutorError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aerogpu.executor.shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var tex0: texture_2d<f32>;
@group(0) @binding(1) var samp0: sampler;

struct VSIn {
  @location(0) pos: vec2<f32>,
};

@vertex
fn vs_main(in: VSIn) -> @builtin(position) vec4<f32> {
  return vec4<f32>(in.pos, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
  // The tests use a 1x1 texture, so the chosen UV doesn't matter.
  return textureSample(tex0, samp0, vec2<f32>(0.5, 0.5));
}
"#
                .into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aerogpu.executor.bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aerogpu.executor.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aerogpu.executor.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            }],
        }];

        let mut pipelines = HashMap::new();
        for fmt in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ] {
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu.executor.pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &vertex_buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: fmt,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
            pipelines.insert(fmt, pipeline);
        }

        Ok(Self {
            device,
            queue,
            buffers: HashMap::new(),
            textures: HashMap::new(),
            state: ExecutorState::default(),
            pipelines,
            bind_group_layout,
            sampler,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn texture(&self, handle: u32) -> Option<&wgpu::Texture> {
        self.textures.get(&handle).map(|t| &t.texture)
    }

    pub fn process_cmd_stream(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> ExecutionReport {
        // Avoid partially executing streams on wasm when `WRITEBACK_DST` is present. The writeback
        // requires `wgpu::Buffer::map_async` completion, which is delivered via the JS event loop
        // and cannot be waited on synchronously.
        #[cfg(target_arch = "wasm32")]
        {
            let stream = match parse_cmd_stream(bytes) {
                Ok(stream) => stream,
                Err(err) => {
                    return ExecutionReport {
                        packets_processed: 0,
                        events: vec![ExecutorEvent::Error {
                            at: 0,
                            message: map_cmd_stream_parse_error(err).to_string(),
                        }],
                    };
                }
            };
            let writeback_at = stream.cmds.iter().position(|cmd| match cmd {
                AeroGpuCmd::CopyBuffer { flags, .. } | AeroGpuCmd::CopyTexture2d { flags, .. } => {
                    (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0
                }
                _ => false,
            });
            if let Some(at) = writeback_at {
                return ExecutionReport {
                    packets_processed: 0,
                    events: vec![ExecutorEvent::Error {
                        at,
                        message: format!(
                            "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async); first WRITEBACK_DST at packet {at}"
                        ),
                    }],
                };
            }
        }

        let mut pending_writebacks = Vec::new();
        match self.execute_cmd_stream_internal(
            bytes,
            guest_memory,
            alloc_table,
            &mut pending_writebacks,
        ) {
            Ok(packets_processed) => {
                if pending_writebacks.is_empty() {
                    ExecutionReport {
                        packets_processed,
                        events: Vec::new(),
                    }
                } else {
                    #[cfg(target_arch = "wasm32")]
                    {
                        ExecutionReport {
                            packets_processed,
                            events: vec![ExecutorEvent::Error {
                                at: 0,
                                message: "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async)".into(),
                            }],
                        }
                    }

                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        match self
                            .flush_pending_writebacks_blocking(pending_writebacks, guest_memory)
                        {
                            Ok(()) => ExecutionReport {
                                packets_processed,
                                events: Vec::new(),
                            },
                            Err(err) => ExecutionReport {
                                packets_processed,
                                events: vec![ExecutorEvent::Error {
                                    at: 0,
                                    message: err.to_string(),
                                }],
                            },
                        }
                    }
                }
            }
            Err((at, err, packets_processed)) => ExecutionReport {
                packets_processed,
                events: vec![ExecutorEvent::Error {
                    at,
                    message: err.to_string(),
                }],
            },
        }
    }

    pub fn process_submission_from_guest_memory(
        &mut self,
        guest_memory: &mut dyn GuestMemory,
        cmd_gpa: u64,
        cmd_size_bytes: u32,
        alloc_table_gpa: u64,
        alloc_table_size_bytes: u32,
    ) -> ExecutionReport {
        let alloc_table = if alloc_table_gpa == 0 && alloc_table_size_bytes == 0 {
            None
        } else {
            match AllocTable::decode_from_guest_memory(
                guest_memory,
                alloc_table_gpa,
                alloc_table_size_bytes,
            ) {
                Ok(table) => Some(table),
                Err(err) => {
                    return ExecutionReport {
                        packets_processed: 0,
                        events: vec![ExecutorEvent::Error {
                            at: 0,
                            message: format!("failed to decode alloc table: {err}"),
                        }],
                    };
                }
            }
        };

        if cmd_gpa == 0 && cmd_size_bytes == 0 {
            return ExecutionReport {
                packets_processed: 0,
                events: Vec::new(),
            };
        }
        if cmd_gpa == 0 || cmd_size_bytes == 0 {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message:
                        "invalid command stream descriptor: cmd_gpa and cmd_size_bytes must be both zero or both non-zero"
                            .into(),
                }],
            };
        }

        const MAX_CMD_STREAM_SIZE_BYTES: u32 = 64 * 1024 * 1024;
        if cmd_gpa.checked_add(u64::from(cmd_size_bytes)).is_none() {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: "command stream gpa+size overflow".into(),
                }],
            };
        }

        // Forward-compat: `cmd_size_bytes` is the backing buffer size, while the command stream
        // header's `size_bytes` indicates how many bytes are actually used.
        let header_size = cmd::AerogpuCmdStreamHeader::SIZE_BYTES as u32;
        if cmd_size_bytes < header_size {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("command stream buffer too small: {cmd_size_bytes} bytes"),
                }],
            };
        }

        let mut header_bytes = [0u8; cmd::AerogpuCmdStreamHeader::SIZE_BYTES];
        if let Err(err) = guest_memory.read(cmd_gpa, &mut header_bytes) {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("failed to read command stream header: {err}"),
                }],
            };
        }

        let header = match cmd::decode_cmd_stream_header_le(&header_bytes) {
            Ok(hdr) => hdr,
            Err(err) => {
                return ExecutionReport {
                    packets_processed: 0,
                    events: vec![ExecutorEvent::Error {
                        at: 0,
                        message: format!("failed to decode command stream header: {err:?}"),
                    }],
                };
            }
        };

        let declared_size_bytes = header.size_bytes;
        if declared_size_bytes > cmd_size_bytes {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!(
                        "command stream header size_bytes too large (size_bytes={declared_size_bytes} > cmd_size_bytes={cmd_size_bytes})"
                    ),
                }],
            };
        }
        if declared_size_bytes > MAX_CMD_STREAM_SIZE_BYTES {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("command stream too large: {declared_size_bytes} bytes"),
                }],
            };
        }

        let cmd_size = declared_size_bytes as usize;
        let mut cmd_bytes = Vec::<u8>::new();
        if cmd_bytes.try_reserve_exact(cmd_size).is_err() {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!(
                        "failed to allocate command stream buffer of size {declared_size_bytes} bytes"
                    ),
                }],
            };
        }
        cmd_bytes.resize(cmd_size, 0u8);
        if let Err(err) = guest_memory.read(cmd_gpa, &mut cmd_bytes) {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("failed to read command stream bytes: {err}"),
                }],
            };
        }

        match alloc_table.as_ref() {
            Some(table) => self.process_cmd_stream(&cmd_bytes, guest_memory, Some(table)),
            None => self.process_cmd_stream(&cmd_bytes, guest_memory, None),
        }
    }

    pub fn execute_cmd_stream(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        #[cfg(target_arch = "wasm32")]
        {
            let stream = parse_cmd_stream(bytes).map_err(map_cmd_stream_parse_error)?;
            let writeback_at = stream.cmds.iter().position(|cmd| match cmd {
                AeroGpuCmd::CopyBuffer { flags, .. } | AeroGpuCmd::CopyTexture2d { flags, .. } => {
                    (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0
                }
                _ => false,
            });
            if let Some(at) = writeback_at {
                return Err(ExecutorError::Validation(
                    format!(
                        "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async); first WRITEBACK_DST at packet {at}"
                    ),
                ));
            }
        }

        let mut pending_writebacks = Vec::new();
        self.execute_cmd_stream_internal(bytes, guest_memory, alloc_table, &mut pending_writebacks)
            .map(|_| ())
            .map_err(|(_, err, _)| err)?;

        if pending_writebacks.is_empty() {
            Ok(())
        } else {
            #[cfg(target_arch = "wasm32")]
            {
                Err(ExecutorError::Validation(
                    "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async)"
                        .into(),
                ))
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.flush_pending_writebacks_blocking(pending_writebacks, guest_memory)?;
                Ok(())
            }
        }
    }

    /// WASM-friendly async variant of `execute_cmd_stream`.
    ///
    /// On WASM targets, `wgpu::Buffer::map_async` completion is delivered via the JS event loop,
    /// so synchronous waiting would deadlock. This method awaits writeback staging buffer maps
    /// when `AEROGPU_COPY_FLAG_WRITEBACK_DST` is used.
    pub async fn execute_cmd_stream_async(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let mut pending_writebacks = Vec::new();
        self.execute_cmd_stream_internal(bytes, guest_memory, alloc_table, &mut pending_writebacks)
            .map(|_| ())
            .map_err(|(_, err, _)| err)?;

        if !pending_writebacks.is_empty() {
            self.flush_pending_writebacks_async(pending_writebacks, guest_memory)
                .await?;
        }

        Ok(())
    }

    fn execute_cmd_stream_internal(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<u32, (usize, ExecutorError, u32)> {
        let stream =
            parse_cmd_stream(bytes).map_err(|err| (0, map_cmd_stream_parse_error(err), 0))?;

        let mut packets_processed = 0u32;
        for cmd in stream.cmds {
            let result = match cmd {
                AeroGpuCmd::CreateBuffer {
                    buffer_handle,
                    usage_flags,
                    size_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                } => self.exec_create_buffer(
                    buffer_handle,
                    usage_flags,
                    size_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                    alloc_table,
                ),
                AeroGpuCmd::CreateTexture2d {
                    texture_handle,
                    usage_flags,
                    format,
                    width,
                    height,
                    mip_levels,
                    array_layers,
                    row_pitch_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                } => self.exec_create_texture2d(
                    CreateTexture2dArgs {
                        texture_handle,
                        usage_flags,
                        format,
                        width,
                        height,
                        mip_levels,
                        array_layers,
                        row_pitch_bytes,
                        backing_alloc_id,
                        backing_offset_bytes,
                    },
                    alloc_table,
                ),
                AeroGpuCmd::DestroyResource { resource_handle } => {
                    self.exec_destroy_resource(resource_handle)
                }
                AeroGpuCmd::ResourceDirtyRange {
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                } => self.exec_resource_dirty_range(
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                    alloc_table,
                ),
                AeroGpuCmd::UploadResource {
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                    data,
                } => self.exec_upload_resource(resource_handle, offset_bytes, size_bytes, data),
                AeroGpuCmd::CopyBuffer {
                    dst_buffer,
                    src_buffer,
                    dst_offset_bytes,
                    src_offset_bytes,
                    size_bytes,
                    flags,
                } => self.exec_copy_buffer(
                    CopyBufferArgs {
                        dst_buffer,
                        src_buffer,
                        dst_offset_bytes,
                        src_offset_bytes,
                        size_bytes,
                        flags,
                    },
                    guest_memory,
                    alloc_table,
                    pending_writebacks,
                ),
                AeroGpuCmd::CopyTexture2d {
                    dst_texture,
                    src_texture,
                    dst_mip_level,
                    dst_array_layer,
                    src_mip_level,
                    src_array_layer,
                    dst_x,
                    dst_y,
                    src_x,
                    src_y,
                    width,
                    height,
                    flags,
                } => self.exec_copy_texture2d(
                    CopyTexture2dArgs {
                        dst_texture,
                        src_texture,
                        dst_mip_level,
                        dst_array_layer,
                        src_mip_level,
                        src_array_layer,
                        dst_x,
                        dst_y,
                        src_x,
                        src_y,
                        width,
                        height,
                        flags,
                    },
                    guest_memory,
                    alloc_table,
                    pending_writebacks,
                ),
                AeroGpuCmd::SetRenderTargets {
                    color_count,
                    depth_stencil,
                    colors,
                } => self.exec_set_render_targets(color_count, depth_stencil, colors),
                AeroGpuCmd::SetVertexBuffers {
                    start_slot,
                    buffer_count,
                    bindings_bytes,
                } => self.exec_set_vertex_buffers(start_slot, buffer_count, bindings_bytes),
                AeroGpuCmd::SetIndexBuffer {
                    buffer,
                    format,
                    offset_bytes,
                } => self.exec_set_index_buffer(buffer, format, offset_bytes),
                AeroGpuCmd::SetTexture {
                    shader_stage,
                    slot,
                    texture,
                } => self.exec_set_texture(shader_stage, slot, texture),
                AeroGpuCmd::Clear {
                    flags,
                    color_rgba_f32,
                    depth_f32,
                    stencil,
                } => self.exec_clear(flags, color_rgba_f32, depth_f32, stencil),
                AeroGpuCmd::Draw {
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                } => self.exec_draw(
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                    guest_memory,
                    alloc_table,
                ),
                AeroGpuCmd::DrawIndexed {
                    index_count,
                    instance_count,
                    first_index,
                    base_vertex,
                    first_instance,
                } => self.exec_draw_indexed(
                    DrawIndexedArgs {
                        index_count,
                        instance_count,
                        first_index,
                        base_vertex,
                        first_instance,
                    },
                    guest_memory,
                    alloc_table,
                ),
                _ => Ok(()),
            };

            match result {
                Ok(()) => packets_processed += 1,
                Err(err) => {
                    // Drop partially-recorded work, but still flush any pending `queue.write_*`
                    // uploads so they don't remain queued indefinitely and reorder with later
                    // submissions.
                    self.queue.submit([]);
                    return Err((packets_processed as usize, err, packets_processed));
                }
            }
        }

        Ok(packets_processed)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn flush_pending_writebacks_blocking(
        &self,
        pending: Vec<PendingWriteback>,
        guest_memory: &mut dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let len: usize = size_bytes.try_into().map_err(|_| {
                        ExecutorError::Validation("COPY_BUFFER: size_bytes out of range".into())
                    })?;
                    let bytes =
                        self.read_buffer_to_vec_blocking(&staging, size_bytes, "COPY_BUFFER")?;
                    if bytes.len() != len {
                        return Err(ExecutorError::Validation(
                            "COPY_BUFFER: internal writeback size mismatch".into(),
                        ));
                    }
                    guest_memory.write(dst_gpa, &bytes)?;
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let staging_size = u64::from(plan.padded_bytes_per_row)
                        .checked_mul(u64::from(plan.rows))
                        .ok_or_else(|| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: staging size overflow".into(),
                            )
                        })?;
                    let mut bytes =
                        self.read_buffer_to_vec_blocking(&staging, staging_size, "COPY_TEXTURE2D")?;

                    let row_bytes_usize: usize =
                        plan.unpadded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: row size out of range".into(),
                            )
                        })?;
                    let padded_bpr_usize: usize =
                        plan.padded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: bytes_per_row out of range".into(),
                            )
                        })?;

                    if plan.is_x8 {
                        for row in 0..plan.rows as usize {
                            let start = row * padded_bpr_usize;
                            let end = start + row_bytes_usize;
                            force_opaque_alpha_rgba8(bytes.get_mut(start..end).ok_or_else(
                                || ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into()),
                            )?);
                        }
                    }

                    for row in 0..plan.rows as u64 {
                        let src_off = row as usize * padded_bpr_usize;
                        let src_end = src_off + row_bytes_usize;
                        let row_bytes_slice = bytes.get(src_off..src_end).ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into())
                        })?;
                        let dst_gpa = plan
                            .base_gpa
                            .checked_add(row.checked_mul(plan.row_pitch).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch mul)".into(),
                                )
                            })?)
                            .ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch add)".into(),
                                )
                            })?;
                        guest_memory.write(dst_gpa, row_bytes_slice)?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn flush_pending_writebacks_async(
        &self,
        pending: Vec<PendingWriteback>,
        guest_memory: &mut dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let len: usize = size_bytes.try_into().map_err(|_| {
                        ExecutorError::Validation("COPY_BUFFER: size_bytes out of range".into())
                    })?;
                    let bytes = self
                        .read_buffer_to_vec_async(&staging, size_bytes, "COPY_BUFFER")
                        .await?;
                    if bytes.len() != len {
                        return Err(ExecutorError::Validation(
                            "COPY_BUFFER: internal writeback size mismatch".into(),
                        ));
                    }
                    guest_memory.write(dst_gpa, &bytes)?;
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let staging_size = u64::from(plan.padded_bytes_per_row)
                        .checked_mul(u64::from(plan.rows))
                        .ok_or_else(|| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: staging size overflow".into(),
                            )
                        })?;
                    let mut bytes = self
                        .read_buffer_to_vec_async(&staging, staging_size, "COPY_TEXTURE2D")
                        .await?;

                    let row_bytes_usize: usize =
                        plan.unpadded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: row size out of range".into(),
                            )
                        })?;
                    let padded_bpr_usize: usize =
                        plan.padded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: bytes_per_row out of range".into(),
                            )
                        })?;

                    if plan.is_x8 {
                        for row in 0..plan.rows as usize {
                            let start = row * padded_bpr_usize;
                            let end = start + row_bytes_usize;
                            force_opaque_alpha_rgba8(bytes.get_mut(start..end).ok_or_else(
                                || ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into()),
                            )?);
                        }
                    }

                    for row in 0..plan.rows as u64 {
                        let src_off = row as usize * padded_bpr_usize;
                        let src_end = src_off + row_bytes_usize;
                        let row_bytes_slice = bytes.get(src_off..src_end).ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into())
                        })?;
                        let dst_gpa = plan
                            .base_gpa
                            .checked_add(row.checked_mul(plan.row_pitch).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch mul)".into(),
                                )
                            })?)
                            .ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch add)".into(),
                                )
                            })?;
                        guest_memory.write(dst_gpa, row_bytes_slice)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn exec_create_buffer(
        &mut self,
        buffer_handle: u32,
        usage_flags: u32,
        size_bytes: u64,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        if buffer_handle == 0 {
            return Err(ExecutorError::Validation(
                "CREATE_BUFFER buffer_handle must be non-zero".into(),
            ));
        }
        if self.textures.contains_key(&buffer_handle) {
            return Err(ExecutorError::Validation(format!(
                "CREATE_BUFFER handle {buffer_handle} is already bound to a texture"
            )));
        }

        if size_bytes == 0 {
            return Err(ExecutorError::Validation(
                "CREATE_BUFFER size_bytes must be > 0".into(),
            ));
        }
        if !size_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT) {
            return Err(ExecutorError::Validation(format!(
                "CREATE_BUFFER size_bytes must be a multiple of {} (got {size_bytes})",
                wgpu::COPY_BUFFER_ALIGNMENT
            )));
        }

        let backing = if backing_alloc_id == 0 {
            None
        } else {
            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_BUFFER backing_alloc_id={backing_alloc_id} requires alloc_table"
                ))
            })?;
            let entry = table.get(backing_alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_BUFFER unknown backing_alloc_id={backing_alloc_id}"
                ))
            })?;

            let backing_offset = u64::from(backing_offset_bytes);
            let end = backing_offset
                .checked_add(size_bytes)
                .ok_or_else(|| ExecutorError::Validation("buffer backing range overflow".into()))?;
            if end > entry.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_BUFFER backing range out of bounds (offset=0x{backing_offset:x}, size=0x{size_bytes:x}, alloc_size=0x{:x})",
                    entry.size_bytes
                )));
            }

            Some(GuestBufferBacking {
                alloc_id: backing_alloc_id,
                alloc_offset_bytes: backing_offset,
            })
        };

        let mut wgpu_usage = wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
        if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER) != 0 {
            wgpu_usage |= wgpu::BufferUsages::VERTEX;
        }
        if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_INDEX_BUFFER) != 0 {
            wgpu_usage |= wgpu::BufferUsages::INDEX;
        }
        if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER) != 0 {
            wgpu_usage |= wgpu::BufferUsages::UNIFORM;
        }

        if let Some(existing) = self.buffers.get_mut(&buffer_handle) {
            if existing.size_bytes != size_bytes || existing.usage_flags != usage_flags {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_* for existing handle {buffer_handle} has mismatched immutable properties; destroy and recreate the handle"
                )));
            }
            existing.backing = backing;
            return Ok(());
        }

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu.executor.buffer"),
            size: size_bytes,
            usage: wgpu_usage,
            mapped_at_creation: false,
        });

        self.buffers.insert(
            buffer_handle,
            BufferResource {
                buffer,
                size_bytes,
                usage_flags,
                backing,
                dirty_ranges: Vec::new(),
            },
        );
        Ok(())
    }

    fn map_format(
        &self,
        format: u32,
    ) -> Result<(wgpu::TextureFormat, TextureUploadTransform), ExecutorError> {
        let bc_enabled = self
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);

        match format {
            v if v == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                || v == pci::AerogpuFormat::B8G8R8X8Unorm as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Bgra8Unorm,
                    TextureUploadTransform::Direct,
                ))
            }
            v if v == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
                || v == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Bgra8UnormSrgb,
                    TextureUploadTransform::Direct,
                ))
            }
            v if v == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                || v == pci::AerogpuFormat::R8G8B8X8Unorm as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Rgba8Unorm,
                    TextureUploadTransform::Direct,
                ))
            }
            v if v == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32
                || v == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    TextureUploadTransform::Direct,
                ))
            }

            // BC formats: sample/upload directly when BC compression is enabled on the device,
            // otherwise CPU-decompress into RGBA8 (to avoid requiring BC sampling support).
            v if v == pci::AerogpuFormat::BC1RgbaUnorm as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc1RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc1ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC1RgbaUnormSrgb as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc1RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc1ToRgba8,
                    ))
                }
            }

            v if v == pci::AerogpuFormat::BC2RgbaUnorm as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc2RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc2ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC2RgbaUnormSrgb as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc2RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc2ToRgba8,
                    ))
                }
            }

            v if v == pci::AerogpuFormat::BC3RgbaUnorm as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc3RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc3ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC3RgbaUnormSrgb as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc3RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc3ToRgba8,
                    ))
                }
            }

            v if v == pci::AerogpuFormat::BC7RgbaUnorm as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc7RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc7ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC7RgbaUnormSrgb as u32 => {
                if bc_enabled {
                    Ok((
                        wgpu::TextureFormat::Bc7RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc7ToRgba8,
                    ))
                }
            }

            _ => Err(ExecutorError::Validation(format!(
                "unsupported aerogpu_format={format}"
            ))),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_create_texture2d(
        &mut self,
        args: CreateTexture2dArgs,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let CreateTexture2dArgs {
            texture_handle,
            usage_flags,
            format,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
            backing_alloc_id,
            backing_offset_bytes,
        } = args;

        if texture_handle == 0 {
            return Err(ExecutorError::Validation(
                "CREATE_TEXTURE2D texture_handle must be non-zero".into(),
            ));
        }
        if self.buffers.contains_key(&texture_handle) {
            return Err(ExecutorError::Validation(format!(
                "CREATE_TEXTURE2D handle {texture_handle} is already bound to a buffer"
            )));
        }

        if mip_levels != 1 || array_layers != 1 {
            return Err(ExecutorError::Validation(format!(
                "only mip_levels=1 and array_layers=1 are supported for now (got mip_levels={mip_levels}, array_layers={array_layers})"
            )));
        }

        let (wgpu_format, upload_transform) = self.map_format(format)?;
        let layout = texture_copy_layout(width, height, format)?;

        if row_pitch_bytes != 0 && row_pitch_bytes < layout.unpadded_bytes_per_row {
            return Err(ExecutorError::Validation(format!(
                "CREATE_TEXTURE2D row_pitch_bytes={row_pitch_bytes} smaller than minimum row size {}",
                layout.unpadded_bytes_per_row
            )));
        }
        let layout_row_pitch_bytes = if row_pitch_bytes != 0 {
            row_pitch_bytes
        } else {
            layout.unpadded_bytes_per_row
        };

        let backing = if backing_alloc_id == 0 {
            None
        } else {
            if row_pitch_bytes == 0 {
                return Err(ExecutorError::Validation(
                    "CREATE_TEXTURE2D row_pitch_bytes must be non-zero when backing_alloc_id != 0"
                        .into(),
                ));
            }

            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D backing_alloc_id={backing_alloc_id} requires alloc_table"
                ))
            })?;
            let entry = table.get(backing_alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D unknown backing_alloc_id={backing_alloc_id}"
                ))
            })?;

            let backing_offset = u64::from(backing_offset_bytes);
            let required_bytes = u64::from(row_pitch_bytes)
                .checked_mul(u64::from(layout.rows_in_layout))
                .ok_or_else(|| ExecutorError::Validation("texture backing size overflow".into()))?;
            let end = backing_offset.checked_add(required_bytes).ok_or_else(|| {
                ExecutorError::Validation("texture backing range overflow".into())
            })?;
            if end > entry.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D backing range out of bounds (offset=0x{backing_offset:x}, size=0x{required_bytes:x}, alloc_size=0x{:x})",
                    entry.size_bytes
                )));
            }

            Some(GuestTextureBacking {
                alloc_id: backing_alloc_id,
                alloc_offset_bytes: backing_offset,
                row_pitch_bytes,
                size_bytes: required_bytes,
            })
        };

        if let Some(existing) = self.textures.get_mut(&texture_handle) {
            if existing.usage_flags != usage_flags
                || existing.format_raw != format
                || existing.width != width
                || existing.height != height
                || existing.row_pitch_bytes != row_pitch_bytes
            {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_* for existing handle {texture_handle} has mismatched immutable properties; destroy and recreate the handle"
                )));
            }

            existing.backing = backing;
            return Ok(());
        }

        let mut usage = wgpu::TextureUsages::empty();
        if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_TEXTURE) != 0 {
            usage |= wgpu::TextureUsages::TEXTURE_BINDING;
        }
        if (usage_flags
            & (cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET
                | cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL))
            != 0
        {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        // Conservative: allow queue.write_texture and readback in tests.
        usage |= wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu.executor.texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.textures.insert(
            texture_handle,
            TextureResource {
                texture,
                view,
                usage_flags,
                format_raw: format,
                row_pitch_bytes,
                width,
                height,
                format: wgpu_format,
                upload_transform,
                layout_row_pitch_bytes,
                backing,
                dirty_ranges: Vec::new(),
            },
        );
        Ok(())
    }

    fn exec_destroy_resource(&mut self, handle: u32) -> Result<(), ExecutorError> {
        self.buffers.remove(&handle);
        self.textures.remove(&handle);
        if self.state.render_target == Some(handle) {
            self.state.render_target = None;
        }
        if self.state.vertex_buffer.map(|v| v.buffer) == Some(handle) {
            self.state.vertex_buffer = None;
        }
        if self.state.index_buffer.map(|v| v.buffer) == Some(handle) {
            self.state.index_buffer = None;
        }
        if self.state.pixel_texture0 == Some(handle) {
            self.state.pixel_texture0 = None;
        }
        Ok(())
    }

    fn exec_resource_dirty_range(
        &mut self,
        handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        if size_bytes == 0 {
            return Ok(());
        }

        if let Some(buffer) = self.buffers.get_mut(&handle) {
            if let Some(backing) = buffer.backing {
                // Validate that the current submission contains an allocation-table entry
                // for this alloc_id. This catches "tracked allocation then split" bugs
                // where the command references guest memory without an alloc-table entry.
                alloc_table
                    .and_then(|t| t.get(backing.alloc_id))
                    .ok_or_else(|| {
                        ExecutorError::Validation(format!(
                            "RESOURCE_DIRTY_RANGE missing alloc table entry for alloc_id={}",
                            backing.alloc_id
                        ))
                    })?;
            } else {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE on host-owned buffer {handle} is not supported (use UPLOAD_RESOURCE)"
                )));
            }
            let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("RESOURCE_DIRTY_RANGE buffer range overflow".into())
            })?;
            if end > buffer.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE out of bounds for buffer {handle} (offset=0x{offset_bytes:x}, size=0x{size_bytes:x}, buffer_size=0x{:x})",
                    buffer.size_bytes
                )));
            }
            let aligned_start = align_down_u64(offset_bytes, wgpu::COPY_BUFFER_ALIGNMENT);
            let aligned_end = align_up_u64(end, wgpu::COPY_BUFFER_ALIGNMENT)?;
            buffer.dirty_ranges.push(aligned_start..aligned_end);
            coalesce_ranges(&mut buffer.dirty_ranges);
            return Ok(());
        }

        if let Some(tex) = self.textures.get_mut(&handle) {
            let Some(backing) = tex.backing else {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE on host-owned texture {handle} is not supported (use UPLOAD_RESOURCE)"
                )));
            };
            alloc_table
                .and_then(|t| t.get(backing.alloc_id))
                .ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "RESOURCE_DIRTY_RANGE missing alloc table entry for alloc_id={}",
                        backing.alloc_id
                    ))
                })?;
            let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("RESOURCE_DIRTY_RANGE texture range overflow".into())
            })?;
            if end > backing.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE out of bounds for texture {handle} (offset=0x{offset_bytes:x}, size=0x{size_bytes:x}, backing_size=0x{:x})",
                    backing.size_bytes
                )));
            }
            tex.dirty_ranges.push(offset_bytes..end);
            coalesce_ranges(&mut tex.dirty_ranges);
            return Ok(());
        }

        Err(ExecutorError::Validation(format!(
            "RESOURCE_DIRTY_RANGE for unknown resource {handle}"
        )))
    }

    fn exec_upload_resource(
        &mut self,
        handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
        data: &[u8],
    ) -> Result<(), ExecutorError> {
        if size_bytes == 0 {
            return Ok(());
        }

        let data_len = usize::try_from(size_bytes).map_err(|_| {
            ExecutorError::Validation("UPLOAD_RESOURCE size_bytes too large".into())
        })?;
        if data.len() != data_len {
            return Err(ExecutorError::Validation(format!(
                "UPLOAD_RESOURCE payload size mismatch (expected {data_len}, found {})",
                data.len()
            )));
        }

        if let Some(buffer) = self.buffers.get_mut(&handle) {
            if buffer.backing.is_some() {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE on guest-backed buffer {handle} is not supported (use RESOURCE_DIRTY_RANGE)"
                )));
            }

            if !offset_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                || !size_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE buffer offset_bytes and size_bytes must be multiples of {} (handle={handle} offset_bytes={offset_bytes} size_bytes={size_bytes})",
                    wgpu::COPY_BUFFER_ALIGNMENT
                )));
            }

            let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("UPLOAD_RESOURCE buffer range overflow".into())
            })?;
            if end > buffer.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE out of bounds for buffer {handle} (offset=0x{offset_bytes:x}, size=0x{size_bytes:x}, buffer_size=0x{:x})",
                    buffer.size_bytes
                )));
            }

            self.queue.write_buffer(&buffer.buffer, offset_bytes, data);
            return Ok(());
        }

        if let Some(tex) = self.textures.get_mut(&handle) {
            if tex.backing.is_some() {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE on guest-backed texture {handle} is not supported (use RESOURCE_DIRTY_RANGE)"
                )));
            }

            let row_pitch = u64::from(tex.layout_row_pitch_bytes);
            if row_pitch == 0 {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE texture {handle} is missing row_pitch_bytes"
                )));
            }

            if !offset_bytes.is_multiple_of(row_pitch) || !size_bytes.is_multiple_of(row_pitch) {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE for texture {handle} must be row-aligned (offset_bytes and size_bytes must be multiples of row_pitch_bytes={})",
                    tex.layout_row_pitch_bytes
                )));
            }

            let start_row = (offset_bytes / row_pitch) as u32;
            let row_count = (size_bytes / row_pitch) as u32;
            let end_row = start_row.saturating_add(row_count);
            let full_layout = texture_copy_layout(tex.width, tex.height, tex.format_raw)?;
            if end_row > full_layout.rows_in_layout {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE out of bounds for texture {handle} (rows {start_row}..{end_row}, rows_in_layout={})",
                    full_layout.rows_in_layout
                )));
            }

            let origin_y = if is_bc_format(tex.format_raw) {
                start_row.checked_mul(4).ok_or_else(|| {
                    ExecutorError::Validation("UPLOAD_RESOURCE: origin.y overflow".into())
                })?
            } else {
                start_row
            };
            let remaining_height = tex.height.checked_sub(origin_y).ok_or_else(|| {
                ExecutorError::Validation("UPLOAD_RESOURCE: origin.y out of bounds".into())
            })?;
            let max_height = if is_bc_format(tex.format_raw) {
                row_count.checked_mul(4).ok_or_else(|| {
                    ExecutorError::Validation("UPLOAD_RESOURCE: height overflow".into())
                })?
            } else {
                row_count
            };
            let copy_height = remaining_height.min(max_height);

            // Direct path uploads the bytes as-is (with row repacking for 256-byte alignment).
            // BC fallback path decompresses into RGBA8 first.
            let mut owned_bytes = Vec::<u8>::new();
            let (bytes, bytes_per_row, rows_per_image, extent_height) = match tex.upload_transform {
                TextureUploadTransform::Direct => {
                    let layout = texture_copy_layout(tex.width, copy_height, tex.format_raw)?;
                    if tex.layout_row_pitch_bytes < layout.unpadded_bytes_per_row {
                        return Err(ExecutorError::Validation(format!(
                            "UPLOAD_RESOURCE texture row_pitch_bytes={} smaller than minimum row size {}",
                            tex.layout_row_pitch_bytes, layout.unpadded_bytes_per_row
                        )));
                    }

                    let upload_bpr = layout.padded_bytes_per_row;
                    let is_x8 = is_x8_format(tex.format_raw);
                    let needs_repack = upload_bpr != tex.layout_row_pitch_bytes || is_x8;

                    if needs_repack {
                        owned_bytes.resize(upload_bpr as usize * row_count as usize, 0);
                        for row in 0..row_count as usize {
                            let src_start = row * tex.layout_row_pitch_bytes as usize;
                            let src_end = src_start + layout.unpadded_bytes_per_row as usize;
                            let dst_start = row * upload_bpr as usize;
                            owned_bytes
                                [dst_start..dst_start + layout.unpadded_bytes_per_row as usize]
                                .copy_from_slice(&data[src_start..src_end]);

                            if is_x8 {
                                force_opaque_alpha_rgba8(
                                    &mut owned_bytes[dst_start
                                        ..dst_start + layout.unpadded_bytes_per_row as usize],
                                );
                            }
                        }
                        (
                            owned_bytes.as_slice(),
                            upload_bpr,
                            // For BC formats, `rows_per_image` is expressed in block rows.
                            // For uncompressed formats, it is expressed in texel rows.
                            row_count,
                            copy_height,
                        )
                    } else {
                        (data, upload_bpr, row_count, copy_height)
                    }
                }
                TextureUploadTransform::Bc1ToRgba8
                | TextureUploadTransform::Bc2ToRgba8
                | TextureUploadTransform::Bc3ToRgba8
                | TextureUploadTransform::Bc7ToRgba8 => {
                    // Strip any per-row padding and decompress into RGBA8.
                    let bc_layout = texture_copy_layout(tex.width, copy_height, tex.format_raw)?;
                    let mut packed_bc =
                        vec![0u8; bc_layout.unpadded_bytes_per_row as usize * row_count as usize];
                    for row in 0..row_count as usize {
                        let src_start = row * tex.layout_row_pitch_bytes as usize;
                        let src_end = src_start + bc_layout.unpadded_bytes_per_row as usize;
                        let dst_start = row * bc_layout.unpadded_bytes_per_row as usize;
                        packed_bc[dst_start..dst_start + bc_layout.unpadded_bytes_per_row as usize]
                            .copy_from_slice(&data[src_start..src_end]);
                    }

                    let decompressed = match tex.upload_transform {
                        TextureUploadTransform::Bc1ToRgba8 => {
                            decompress_bc1_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Bc2ToRgba8 => {
                            decompress_bc2_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Bc3ToRgba8 => {
                            decompress_bc3_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Bc7ToRgba8 => {
                            decompress_bc7_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Direct => unreachable!(),
                    };

                    let rgba_layout = texture_copy_layout(
                        tex.width,
                        copy_height,
                        pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                    )?;
                    let upload_bpr = rgba_layout.padded_bytes_per_row;
                    if upload_bpr == rgba_layout.unpadded_bytes_per_row {
                        owned_bytes = decompressed;
                    } else {
                        owned_bytes.resize(upload_bpr as usize * copy_height as usize, 0);
                        for row in 0..copy_height as usize {
                            let src_start = row * rgba_layout.unpadded_bytes_per_row as usize;
                            let src_end = src_start + rgba_layout.unpadded_bytes_per_row as usize;
                            let dst_start = row * upload_bpr as usize;
                            owned_bytes[dst_start
                                ..dst_start + rgba_layout.unpadded_bytes_per_row as usize]
                                .copy_from_slice(&decompressed[src_start..src_end]);
                        }
                    }

                    (owned_bytes.as_slice(), upload_bpr, copy_height, copy_height)
                }
            };

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &tex.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: origin_y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(rows_per_image),
                },
                wgpu::Extent3d {
                    width: tex.width,
                    height: extent_height,
                    depth_or_array_layers: 1,
                },
            );
            return Ok(());
        }

        Err(ExecutorError::Validation(format!(
            "UPLOAD_RESOURCE for unknown resource {handle}"
        )))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_buffer_to_vec_blocking(
        &self,
        buffer: &wgpu::Buffer,
        size_bytes: u64,
        context: &'static str,
    ) -> Result<Vec<u8>, ExecutorError> {
        let slice = buffer.slice(..size_bytes);
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
        let map_res = guard.take().unwrap();
        map_res.map_err(|err| {
            ExecutorError::Validation(format!("{context}: writeback map_async failed: {err:?}"))
        })?;

        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    async fn read_buffer_to_vec_async(
        &self,
        buffer: &wgpu::Buffer,
        size_bytes: u64,
        context: &'static str,
    ) -> Result<Vec<u8>, ExecutorError> {
        let slice = buffer.slice(..size_bytes);
        let (sender, receiver) = oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            sender.send(res).ok();
        });

        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);
        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);

        receiver
            .receive()
            .await
            .ok_or_else(|| {
                ExecutorError::Validation(format!("{context}: map_async sender dropped"))
            })?
            .map_err(|err| {
                ExecutorError::Validation(format!("{context}: map_async failed: {err:?}"))
            })?;

        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_copy_buffer(
        &mut self,
        args: CopyBufferArgs,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<(), ExecutorError> {
        let CopyBufferArgs {
            dst_buffer,
            src_buffer,
            dst_offset_bytes,
            src_offset_bytes,
            size_bytes,
            flags,
        } = args;
        if size_bytes == 0 {
            return Ok(());
        }

        let writeback = (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            return Err(ExecutorError::Validation(format!(
                "COPY_BUFFER: unsupported flags 0x{flags:08X}"
            )));
        }

        if dst_buffer == 0 || src_buffer == 0 {
            return Err(ExecutorError::Validation(
                "COPY_BUFFER: resource handles must be non-zero".into(),
            ));
        }
        if dst_buffer == src_buffer {
            return Err(ExecutorError::Validation(
                "COPY_BUFFER: src==dst is not supported".into(),
            ));
        }

        if !dst_offset_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            || !src_offset_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            || !size_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
        {
            return Err(ExecutorError::Validation(format!(
                "COPY_BUFFER: offsets and size must be {}-byte aligned (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes} size_bytes={size_bytes})",
                wgpu::COPY_BUFFER_ALIGNMENT
            )));
        }

        let (src_size, dst_size, dst_backing) = {
            let src = self.buffers.get(&src_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown src buffer {src_buffer}"))
            })?;
            let dst = self.buffers.get(&dst_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))
            })?;
            let dst_backing = if writeback {
                Some(dst.backing.ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "COPY_BUFFER: WRITEBACK_DST requires dst buffer to be guest-backed (handle={dst_buffer})"
                    ))
                })?)
            } else {
                None
            };
            (src.size_bytes, dst.size_bytes, dst_backing)
        };

        let src_end = src_offset_bytes
            .checked_add(size_bytes)
            .ok_or_else(|| ExecutorError::Validation("COPY_BUFFER: src range overflow".into()))?;
        let dst_end = dst_offset_bytes
            .checked_add(size_bytes)
            .ok_or_else(|| ExecutorError::Validation("COPY_BUFFER: dst range overflow".into()))?;
        if src_end > src_size || dst_end > dst_size {
            return Err(ExecutorError::Validation(
                "COPY_BUFFER: out of bounds".into(),
            ));
        }

        let dst_writeback_gpa = if writeback {
            let dst_backing = dst_backing.ok_or_else(|| {
                ExecutorError::Validation(
                    "COPY_BUFFER: internal error: missing dst guest backing for writeback".into(),
                )
            })?;
            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation("COPY_BUFFER: WRITEBACK_DST requires alloc_table".into())
            })?;
            let entry = table.get(dst_backing.alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_BUFFER: missing alloc table entry for alloc_id={} (dst_buffer={dst_buffer})",
                    dst_backing.alloc_id
                ))
            })?;
            if (entry.flags & ring::AEROGPU_ALLOC_FLAG_READONLY) != 0 {
                return Err(ExecutorError::Validation(format!(
                    "COPY_BUFFER: dst_buffer={dst_buffer} backing alloc_id={} is READONLY",
                    dst_backing.alloc_id
                )));
            }
            let alloc_offset = dst_backing
                .alloc_offset_bytes
                .checked_add(dst_offset_bytes)
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_BUFFER: dst alloc offset overflow".into())
                })?;
            Some(table.resolve_gpa(dst_backing.alloc_id, alloc_offset, size_bytes)?)
        } else {
            None
        };

        // Flush any pending CPU writes before the copy reads/writes the buffers.
        self.flush_buffer_if_dirty(src_buffer, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(dst_buffer, guest_memory, alloc_table)?;

        let (src, dst) = {
            let src = self.buffers.get(&src_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown src buffer {src_buffer}"))
            })?;
            let dst = self.buffers.get(&dst_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))
            })?;
            (&src.buffer, &dst.buffer)
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.copy_buffer.encoder"),
            });
        encoder.copy_buffer_to_buffer(src, src_offset_bytes, dst, dst_offset_bytes, size_bytes);
        let staging = if writeback {
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu.executor.copy_buffer.writeback"),
                size: size_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(dst, dst_offset_bytes, &staging, 0, size_bytes);
            Some(staging)
        } else {
            None
        };
        self.queue.submit([encoder.finish()]);

        if let Some(dst_gpa) = dst_writeback_gpa {
            let Some(staging) = staging else {
                return Err(ExecutorError::Validation(
                    "COPY_BUFFER: missing staging buffer for writeback".into(),
                ));
            };
            pending_writebacks.push(PendingWriteback::Buffer {
                staging,
                dst_gpa,
                size_bytes,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_copy_texture2d(
        &mut self,
        args: CopyTexture2dArgs,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<(), ExecutorError> {
        let CopyTexture2dArgs {
            dst_texture,
            src_texture,
            dst_mip_level,
            dst_array_layer,
            src_mip_level,
            src_array_layer,
            dst_x,
            dst_y,
            src_x,
            src_y,
            width,
            height,
            flags,
        } = args;
        if width == 0 || height == 0 {
            return Ok(());
        }

        let writeback = (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            return Err(ExecutorError::Validation(format!(
                "COPY_TEXTURE2D: unsupported flags 0x{flags:08X}"
            )));
        }

        if dst_texture == 0 || src_texture == 0 {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: resource handles must be non-zero".into(),
            ));
        }
        if dst_texture == src_texture {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: src==dst is not supported".into(),
            ));
        }

        if dst_mip_level != 0 || dst_array_layer != 0 || src_mip_level != 0 || src_array_layer != 0
        {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D only supports mip0 layer0".into(),
            ));
        }

        let (
            src_extent,
            dst_extent,
            src_format_raw,
            dst_format_raw,
            dst_upload_transform,
            dst_backing,
        ) = {
            let src = self.textures.get(&src_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown src texture {src_texture}"
                ))
            })?;
            let dst = self.textures.get(&dst_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown dst texture {dst_texture}"
                ))
            })?;
            let dst_backing = if writeback {
                Some(dst.backing.ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "COPY_TEXTURE2D: WRITEBACK_DST requires dst texture to be guest-backed (handle={dst_texture})"
                    ))
                })?)
            } else {
                None
            };

            (
                (src.width, src.height),
                (dst.width, dst.height),
                src.format_raw,
                dst.format_raw,
                dst.upload_transform,
                dst_backing,
            )
        };

        if src_format_raw != dst_format_raw {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: format mismatch".into(),
            ));
        }
        let dst_is_x8 = is_x8_format(dst_format_raw);

        let src_end_x = src_x
            .checked_add(width)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: src rect overflow".into()))?;
        let src_end_y = src_y
            .checked_add(height)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: src rect overflow".into()))?;
        let dst_end_x = dst_x
            .checked_add(width)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: dst rect overflow".into()))?;
        let dst_end_y = dst_y
            .checked_add(height)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: dst rect overflow".into()))?;

        if src_end_x > src_extent.0 || src_end_y > src_extent.1 {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: src rect out of bounds".into(),
            ));
        }
        if dst_end_x > dst_extent.0 || dst_end_y > dst_extent.1 {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: dst rect out of bounds".into(),
            ));
        }

        if is_bc_format(dst_format_raw) {
            // WebGPU requires compressed texture copies to be block-aligned:
            // - origins must be multiples of 4
            // - sizes must be multiples of 4 unless the copy reaches the mip edge
            if !src_x.is_multiple_of(4)
                || !src_y.is_multiple_of(4)
                || !dst_x.is_multiple_of(4)
                || !dst_y.is_multiple_of(4)
            {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: BC origin must be 4x4 block-aligned (src=({src_x},{src_y}) dst=({dst_x},{dst_y}))"
                )));
            }

            if !width.is_multiple_of(4) && (src_end_x != src_extent.0 || dst_end_x != dst_extent.0)
            {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: BC width must be a multiple of 4 unless the copy reaches the mip edge of both textures (src_end_x={src_end_x} src_width={} dst_end_x={dst_end_x} dst_width={})",
                    src_extent.0, dst_extent.0
                )));
            }
            if !height.is_multiple_of(4) && (src_end_y != src_extent.1 || dst_end_y != dst_extent.1)
            {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: BC height must be a multiple of 4 unless the copy reaches the mip edge of both textures (src_end_y={src_end_y} src_height={} dst_end_y={dst_end_y} dst_height={})",
                    src_extent.1, dst_extent.1
                )));
            }
        }

        let writeback_plan = if writeback {
            let dst_backing = dst_backing.ok_or_else(|| {
                ExecutorError::Validation(
                    "COPY_TEXTURE2D: internal error: missing dst guest backing for writeback"
                        .into(),
                )
            })?;
            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation(
                    "COPY_TEXTURE2D: WRITEBACK_DST requires alloc_table".into(),
                )
            })?;
            let entry = table.get(dst_backing.alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: missing alloc table entry for alloc_id={} (dst_texture={dst_texture})",
                    dst_backing.alloc_id
                ))
            })?;
            if (entry.flags & ring::AEROGPU_ALLOC_FLAG_READONLY) != 0 {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: dst_texture={dst_texture} backing alloc_id={} is READONLY",
                    dst_backing.alloc_id
                )));
            }

            if is_bc_format(dst_format_raw)
                && dst_upload_transform != TextureUploadTransform::Direct
            {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: WRITEBACK_DST for BC textures requires TEXTURE_COMPRESSION_BC"
                        .into(),
                ));
            }

            let region_layout = texture_copy_layout(width, height, dst_format_raw)?;

            let dst_x_blocks = dst_x.checked_div(region_layout.block_w).ok_or_else(|| {
                ExecutorError::Validation("COPY_TEXTURE2D: dst_x div overflow".into())
            })?;
            let dst_y_blocks = dst_y.checked_div(region_layout.block_h).ok_or_else(|| {
                ExecutorError::Validation("COPY_TEXTURE2D: dst_y div overflow".into())
            })?;
            let dst_x_bytes = u64::from(dst_x_blocks)
                .checked_mul(u64::from(region_layout.block_bytes))
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: dst_x overflow".into())
                })?;

            let row_pitch = u64::from(dst_backing.row_pitch_bytes);
            if row_pitch == 0 {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: missing dst row_pitch_bytes for writeback".into(),
                ));
            }

            let start_offset =
                dst_backing
                    .alloc_offset_bytes
                    .checked_add(u64::from(dst_y_blocks).checked_mul(row_pitch).ok_or_else(
                        || ExecutorError::Validation("COPY_TEXTURE2D: dst_y overflow".into()),
                    )?)
                    .and_then(|v| v.checked_add(dst_x_bytes))
                    .ok_or_else(|| {
                        ExecutorError::Validation("COPY_TEXTURE2D: backing overflow".into())
                    })?;

            let last_row_start = start_offset
                .checked_add(
                    u64::from(region_layout.rows_in_layout)
                        .checked_sub(1)
                        .ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: height underflow".into())
                        })?
                        .checked_mul(row_pitch)
                        .ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: row offset overflow".into())
                        })?,
                )
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: row offset overflow".into())
                })?;
            let end_offset = last_row_start
                .checked_add(u64::from(region_layout.unpadded_bytes_per_row))
                .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: end overflow".into()))?;
            let validate_size = end_offset.checked_sub(start_offset).ok_or_else(|| {
                ExecutorError::Validation("COPY_TEXTURE2D: size underflow".into())
            })?;
            let backing_end = dst_backing
                .alloc_offset_bytes
                .checked_add(dst_backing.size_bytes)
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: backing overflow".into())
                })?;
            if end_offset > backing_end {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: writeback out of bounds".into(),
                ));
            }

            let base_gpa = table.resolve_gpa(dst_backing.alloc_id, start_offset, validate_size)?;

            Some(TextureWritebackPlan {
                base_gpa,
                row_pitch,
                rows: region_layout.rows_in_layout,
                is_x8: dst_is_x8,
                padded_bytes_per_row: region_layout.padded_bytes_per_row,
                unpadded_bytes_per_row: region_layout.unpadded_bytes_per_row,
            })
        } else {
            None
        };

        // Flush any pending CPU writes before the copy reads/writes the textures.
        self.flush_texture_if_dirty(src_texture, guest_memory, alloc_table)?;
        self.flush_texture_if_dirty(dst_texture, guest_memory, alloc_table)?;

        let (src, dst) = {
            let src = self.textures.get(&src_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown src texture {src_texture}"
                ))
            })?;
            let dst = self.textures.get(&dst_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown dst texture {dst_texture}"
                ))
            })?;
            (&src.texture, &dst.texture)
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.copy_texture2d.encoder"),
            });
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: src,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: src_x,
                    y: src_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: dst,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: dst_x,
                    y: dst_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let staging = if let Some(plan) = writeback_plan {
            let size_bytes = u64::from(plan.padded_bytes_per_row)
                .checked_mul(u64::from(plan.rows))
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: staging size overflow".into())
                })?;
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu.executor.copy_texture2d.writeback"),
                size: size_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: dst,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: dst_x,
                        y: dst_y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &staging,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(plan.padded_bytes_per_row),
                        rows_per_image: Some(plan.rows),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            Some(staging)
        } else {
            None
        };
        self.queue.submit([encoder.finish()]);

        if let Some(plan) = writeback_plan {
            let Some(staging) = staging else {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: missing staging buffer for writeback".into(),
                ));
            };
            pending_writebacks.push(PendingWriteback::Texture2d { staging, plan });
        }
        Ok(())
    }

    fn exec_set_render_targets(
        &mut self,
        color_count: u32,
        _depth_stencil: u32,
        colors: [u32; cmd::AEROGPU_MAX_RENDER_TARGETS],
    ) -> Result<(), ExecutorError> {
        if color_count > 1 {
            return Err(ExecutorError::Validation(
                "only color_count<=1 is supported".into(),
            ));
        }
        let color0 = colors[0];
        if color_count == 0 || color0 == 0 {
            self.state.render_target = None;
            return Ok(());
        }
        let tex = self.textures.get(&color0).ok_or_else(|| {
            ExecutorError::Validation(format!("SET_RENDER_TARGETS unknown texture {color0}"))
        })?;
        if !self.pipelines.contains_key(&tex.format) {
            return Err(ExecutorError::Validation(format!(
                "render target format {:?} not supported by executor",
                tex.format
            )));
        }
        self.state.render_target = Some(color0);
        Ok(())
    }

    fn exec_set_vertex_buffers(
        &mut self,
        start_slot: u32,
        buffer_count: u32,
        bindings_bytes: &[u8],
    ) -> Result<(), ExecutorError> {
        if start_slot != 0 {
            return Err(ExecutorError::Validation(
                "only start_slot=0 is supported".into(),
            ));
        }
        if buffer_count == 0 {
            self.state.vertex_buffer = None;
            return Ok(());
        }

        let expected_size = (buffer_count as usize)
            .checked_mul(cmd::AerogpuVertexBufferBinding::SIZE_BYTES)
            .ok_or_else(|| {
                ExecutorError::Validation("vertex buffer binding size overflow".into())
            })?;
        if bindings_bytes.len() < expected_size {
            return Err(ExecutorError::TruncatedPacket);
        }

        // Only track slot 0 for now.
        let buffer = read_u32_le(bindings_bytes, 0)?;
        let stride_bytes = read_u32_le(bindings_bytes, 4)?;
        let offset_bytes = read_u32_le(bindings_bytes, 8)?;

        if buffer == 0 {
            self.state.vertex_buffer = None;
            return Ok(());
        }
        if !self.buffers.contains_key(&buffer) {
            return Err(ExecutorError::Validation(format!(
                "SET_VERTEX_BUFFERS unknown buffer {buffer}"
            )));
        }

        self.state.vertex_buffer = Some(BoundVertexBuffer {
            buffer,
            stride_bytes,
            offset_bytes,
        });
        Ok(())
    }

    fn exec_set_index_buffer(
        &mut self,
        buffer: u32,
        format_raw: u32,
        offset_bytes: u32,
    ) -> Result<(), ExecutorError> {
        if buffer == 0 {
            self.state.index_buffer = None;
            return Ok(());
        }

        if !self.buffers.contains_key(&buffer) {
            return Err(ExecutorError::Validation(format!(
                "SET_INDEX_BUFFER unknown buffer {buffer}"
            )));
        }

        let format = match format_raw {
            v if v == cmd::AerogpuIndexFormat::Uint16 as u32 => wgpu::IndexFormat::Uint16,
            v if v == cmd::AerogpuIndexFormat::Uint32 as u32 => wgpu::IndexFormat::Uint32,
            _ => {
                return Err(ExecutorError::Validation(format!(
                    "SET_INDEX_BUFFER unknown index format {format_raw}"
                )))
            }
        };

        let align = match format {
            wgpu::IndexFormat::Uint16 => 2,
            wgpu::IndexFormat::Uint32 => 4,
        };
        if !(offset_bytes as u64).is_multiple_of(align) {
            return Err(ExecutorError::Validation(format!(
                "SET_INDEX_BUFFER offset_bytes must be aligned to {align} (got {offset_bytes})"
            )));
        }

        let buf_size = self.buffers.get(&buffer).unwrap().size_bytes;
        if offset_bytes as u64 > buf_size {
            return Err(ExecutorError::Validation(format!(
                "SET_INDEX_BUFFER offset_bytes {offset_bytes} out of bounds for buffer {buffer} (size={buf_size})"
            )));
        }

        self.state.index_buffer = Some(BoundIndexBuffer {
            buffer,
            format,
            offset_bytes,
        });
        Ok(())
    }

    fn exec_set_texture(
        &mut self,
        _shader_stage: u32,
        slot: u32,
        texture: u32,
    ) -> Result<(), ExecutorError> {
        if slot != 0 {
            return Err(ExecutorError::Validation(
                "only texture slot 0 is supported".into(),
            ));
        }
        if texture == 0 {
            self.state.pixel_texture0 = None;
            return Ok(());
        }
        if !self.textures.contains_key(&texture) {
            return Err(ExecutorError::Validation(format!(
                "SET_TEXTURE unknown texture {texture}"
            )));
        }
        self.state.pixel_texture0 = Some(texture);
        Ok(())
    }

    fn exec_clear(
        &mut self,
        flags: u32,
        color_rgba_f32: [u32; 4],
        _depth_f32: u32,
        _stencil: u32,
    ) -> Result<(), ExecutorError> {
        if flags & cmd::AEROGPU_CLEAR_COLOR == 0 {
            return Ok(());
        }

        let Some(rt) = self.state.render_target else {
            return Err(ExecutorError::Validation(
                "CLEAR requires a bound render target".into(),
            ));
        };
        let rt_tex = self.textures.get(&rt).ok_or_else(|| {
            ExecutorError::Validation(format!("CLEAR render target {rt} missing"))
        })?;

        let r = f32::from_bits(color_rgba_f32[0]);
        let g = f32::from_bits(color_rgba_f32[1]);
        let b = f32::from_bits(color_rgba_f32[2]);
        let a = f32::from_bits(color_rgba_f32[3]);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.clear.encoder"),
            });

        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu.executor.clear.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_tex.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r as f64,
                            g: g as f64,
                            b: b as f64,
                            a: a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
        }

        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn exec_draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let Some(rt) = self.state.render_target else {
            return Err(ExecutorError::Validation(
                "DRAW requires a bound render target".into(),
            ));
        };
        let Some(vb) = self.state.vertex_buffer else {
            return Err(ExecutorError::Validation(
                "DRAW requires a bound vertex buffer".into(),
            ));
        };
        let Some(tex0) = self.state.pixel_texture0 else {
            return Err(ExecutorError::Validation(
                "DRAW requires a bound pixel texture slot 0".into(),
            ));
        };

        // Upload pending dirty ranges for any guest-backed resources used by this draw.
        self.flush_texture_if_dirty(rt, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(vb.buffer, guest_memory, alloc_table)?;
        self.flush_texture_if_dirty(tex0, guest_memory, alloc_table)?;

        let rt_tex = self
            .textures
            .get(&rt)
            .ok_or_else(|| ExecutorError::Validation(format!("DRAW render target {rt} missing")))?;
        let vb_res = self.buffers.get(&vb.buffer).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW vertex buffer {} missing", vb.buffer))
        })?;
        let tex0_res = self
            .textures
            .get(&tex0)
            .ok_or_else(|| ExecutorError::Validation(format!("DRAW texture {tex0} missing")))?;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu.executor.bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex0_res.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.draw.encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu.executor.draw.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_tex.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            let pipeline = self.pipelines.get(&rt_tex.format).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "no pipeline configured for render target format {:?}",
                    rt_tex.format
                ))
            })?;
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(
                0,
                vb_res
                    .buffer
                    .slice(vb.offset_bytes as u64..vb_res.size_bytes),
            );
            // The stride is baked into the pipeline; validate to avoid confusing failures.
            if vb.stride_bytes != 8 {
                return Err(ExecutorError::Validation(format!(
                    "vertex buffer stride_bytes must be 8 for the built-in pipeline (got {})",
                    vb.stride_bytes
                )));
            }
            pass.draw(
                first_vertex..first_vertex.saturating_add(vertex_count),
                first_instance..first_instance.saturating_add(instance_count),
            );
        }

        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_draw_indexed(
        &mut self,
        args: DrawIndexedArgs,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let DrawIndexedArgs {
            index_count,
            instance_count,
            first_index,
            base_vertex,
            first_instance,
        } = args;
        let Some(rt) = self.state.render_target else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound render target".into(),
            ));
        };
        let Some(vb) = self.state.vertex_buffer else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound vertex buffer".into(),
            ));
        };
        let Some(ib) = self.state.index_buffer else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound index buffer".into(),
            ));
        };
        let Some(tex0) = self.state.pixel_texture0 else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound pixel texture slot 0".into(),
            ));
        };

        self.flush_texture_if_dirty(rt, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(vb.buffer, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(ib.buffer, guest_memory, alloc_table)?;
        self.flush_texture_if_dirty(tex0, guest_memory, alloc_table)?;

        let rt_tex = self.textures.get(&rt).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED render target {rt} missing"))
        })?;
        let vb_res = self.buffers.get(&vb.buffer).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED vertex buffer {} missing", vb.buffer))
        })?;
        let ib_res = self.buffers.get(&ib.buffer).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED index buffer {} missing", ib.buffer))
        })?;

        let tex0_res = self.textures.get(&tex0).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED texture {tex0} missing"))
        })?;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu.executor.bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex0_res.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.draw_indexed.encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu.executor.draw_indexed.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_tex.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            let pipeline = self.pipelines.get(&rt_tex.format).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "no pipeline configured for render target format {:?}",
                    rt_tex.format
                ))
            })?;
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(
                0,
                vb_res
                    .buffer
                    .slice(vb.offset_bytes as u64..vb_res.size_bytes),
            );
            if vb.stride_bytes != 8 {
                return Err(ExecutorError::Validation(format!(
                    "vertex buffer stride_bytes must be 8 for the built-in pipeline (got {})",
                    vb.stride_bytes
                )));
            }

            if ib.offset_bytes as u64 > ib_res.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "index buffer offset out of bounds (offset={}, size={})",
                    ib.offset_bytes, ib_res.size_bytes
                )));
            }
            pass.set_index_buffer(
                ib_res
                    .buffer
                    .slice(ib.offset_bytes as u64..ib_res.size_bytes),
                ib.format,
            );

            pass.draw_indexed(
                first_index..first_index.saturating_add(index_count),
                base_vertex,
                first_instance..first_instance.saturating_add(instance_count),
            );
        }

        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn flush_buffer_if_dirty(
        &mut self,
        handle: u32,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let Some(buffer) = self.buffers.get_mut(&handle) else {
            return Err(ExecutorError::Validation(format!(
                "unknown buffer {handle}"
            )));
        };
        let Some(backing) = buffer.backing else {
            // Host-owned buffers are updated through UPLOAD_RESOURCE.
            return Ok(());
        };
        if buffer.dirty_ranges.is_empty() {
            return Ok(());
        }

        let table = alloc_table.ok_or_else(|| {
            ExecutorError::Validation(format!(
                "dirty guest-backed buffer {handle} requires alloc_table"
            ))
        })?;

        for range in &buffer.dirty_ranges {
            let aligned_start = align_down_u64(range.start, wgpu::COPY_BUFFER_ALIGNMENT);
            let aligned_end =
                align_up_u64(range.end, wgpu::COPY_BUFFER_ALIGNMENT)?.min(buffer.size_bytes);
            let len = aligned_end
                .checked_sub(aligned_start)
                .ok_or_else(|| ExecutorError::Validation("invalid dirty range".into()))?;
            let len_usize = usize::try_from(len)
                .map_err(|_| ExecutorError::Validation("buffer dirty range too large".into()))?;
            let mut data = vec![0u8; len_usize];

            let alloc_offset = backing
                .alloc_offset_bytes
                .checked_add(aligned_start)
                .ok_or_else(|| ExecutorError::Validation("buffer alloc offset overflow".into()))?;
            let src_gpa = table.resolve_gpa(backing.alloc_id, alloc_offset, len)?;
            guest_memory.read(src_gpa, &mut data)?;
            self.queue
                .write_buffer(&buffer.buffer, aligned_start, &data);
        }

        buffer.dirty_ranges.clear();
        Ok(())
    }

    fn flush_texture_if_dirty(
        &mut self,
        handle: u32,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let Some(tex) = self.textures.get_mut(&handle) else {
            return Err(ExecutorError::Validation(format!(
                "unknown texture {handle}"
            )));
        };
        let Some(backing) = tex.backing else {
            // Host-owned textures are updated through UPLOAD_RESOURCE.
            return Ok(());
        };
        if tex.dirty_ranges.is_empty() {
            return Ok(());
        }

        let table = alloc_table.ok_or_else(|| {
            ExecutorError::Validation(format!(
                "dirty guest-backed texture {handle} requires alloc_table"
            ))
        })?;

        let row_pitch = backing.row_pitch_bytes as u64;
        if row_pitch == 0 {
            return Err(ExecutorError::Validation(format!(
                "dirty guest-backed texture {handle} has row_pitch_bytes=0"
            )));
        }
        let full_layout = texture_copy_layout(tex.width, tex.height, tex.format_raw)?;
        let mut row_ranges = Vec::<Range<u32>>::new();
        for r in &tex.dirty_ranges {
            let start_row = (r.start / row_pitch) as u32;
            let end_row = r.end.div_ceil(row_pitch) as u32;
            row_ranges.push(start_row..end_row);
        }
        coalesce_ranges_u32(&mut row_ranges);
        for rows in &row_ranges {
            if rows.end > full_layout.rows_in_layout {
                return Err(ExecutorError::Validation(format!(
                    "computed dirty row range {rows:?} exceeds rows_in_layout {}",
                    full_layout.rows_in_layout
                )));
            }
        }

        for rows in row_ranges {
            let row_count = rows.end.saturating_sub(rows.start);
            if row_count == 0 {
                continue;
            }

            let origin_y = if is_bc_format(tex.format_raw) {
                rows.start.checked_mul(4).ok_or_else(|| {
                    ExecutorError::Validation("RESOURCE_DIRTY_RANGE: origin.y overflow".into())
                })?
            } else {
                rows.start
            };
            let remaining_height = tex.height.checked_sub(origin_y).ok_or_else(|| {
                ExecutorError::Validation("RESOURCE_DIRTY_RANGE: origin.y out of bounds".into())
            })?;
            let max_height = if is_bc_format(tex.format_raw) {
                row_count.checked_mul(4).ok_or_else(|| {
                    ExecutorError::Validation("RESOURCE_DIRTY_RANGE: height overflow".into())
                })?
            } else {
                row_count
            };
            let copy_height = remaining_height.min(max_height);

            match tex.upload_transform {
                TextureUploadTransform::Direct => {
                    let layout = texture_copy_layout(tex.width, copy_height, tex.format_raw)?;
                    let upload_bpr = layout.padded_bytes_per_row;
                    let is_x8 = is_x8_format(tex.format_raw);

                    let mut staging = vec![0u8; upload_bpr as usize * row_count as usize];
                    for i in 0..row_count {
                        let row = rows.start + i;
                        let row_off = (row as u64).checked_mul(row_pitch).ok_or_else(|| {
                            ExecutorError::Validation("texture row offset overflow".into())
                        })?;
                        let alloc_offset = backing
                            .alloc_offset_bytes
                            .checked_add(row_off)
                            .ok_or_else(|| {
                                ExecutorError::Validation("texture alloc offset overflow".into())
                            })?;
                        let src_gpa = table.resolve_gpa(
                            backing.alloc_id,
                            alloc_offset,
                            layout.unpadded_bytes_per_row as u64,
                        )?;
                        let dst_off = i as usize * upload_bpr as usize;
                        guest_memory.read(
                            src_gpa,
                            &mut staging[dst_off..dst_off + layout.unpadded_bytes_per_row as usize],
                        )?;
                        if is_x8 {
                            force_opaque_alpha_rgba8(
                                &mut staging
                                    [dst_off..dst_off + layout.unpadded_bytes_per_row as usize],
                            );
                        }
                    }

                    self.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &tex.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: origin_y,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &staging,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(upload_bpr),
                            rows_per_image: Some(row_count),
                        },
                        wgpu::Extent3d {
                            width: tex.width,
                            height: copy_height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                TextureUploadTransform::Bc1ToRgba8
                | TextureUploadTransform::Bc2ToRgba8
                | TextureUploadTransform::Bc3ToRgba8
                | TextureUploadTransform::Bc7ToRgba8 => {
                    // BC CPU fallback: read compressed bytes for the block rows in the dirty range,
                    // decompress, and upload as RGBA8.
                    let bc_layout = texture_copy_layout(tex.width, copy_height, tex.format_raw)?;

                    let mut packed_bc =
                        vec![0u8; bc_layout.unpadded_bytes_per_row as usize * row_count as usize];
                    for i in 0..row_count {
                        let row = rows.start + i;
                        let row_off = (row as u64).checked_mul(row_pitch).ok_or_else(|| {
                            ExecutorError::Validation("texture row offset overflow".into())
                        })?;
                        let alloc_offset = backing
                            .alloc_offset_bytes
                            .checked_add(row_off)
                            .ok_or_else(|| {
                                ExecutorError::Validation("texture alloc offset overflow".into())
                            })?;
                        let src_gpa = table.resolve_gpa(
                            backing.alloc_id,
                            alloc_offset,
                            bc_layout.unpadded_bytes_per_row as u64,
                        )?;
                        let dst_off = i as usize * bc_layout.unpadded_bytes_per_row as usize;
                        guest_memory.read(
                            src_gpa,
                            &mut packed_bc
                                [dst_off..dst_off + bc_layout.unpadded_bytes_per_row as usize],
                        )?;
                    }

                    let decompressed = match tex.upload_transform {
                        TextureUploadTransform::Bc1ToRgba8 => {
                            decompress_bc1_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Bc2ToRgba8 => {
                            decompress_bc2_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Bc3ToRgba8 => {
                            decompress_bc3_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Bc7ToRgba8 => {
                            decompress_bc7_rgba8(tex.width, copy_height, &packed_bc)
                        }
                        TextureUploadTransform::Direct => unreachable!(),
                    };

                    let rgba_layout = texture_copy_layout(
                        tex.width,
                        copy_height,
                        pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                    )?;
                    let upload_bpr = rgba_layout.padded_bytes_per_row;

                    let mut staging = vec![0u8; upload_bpr as usize * copy_height as usize];
                    for y in 0..copy_height as usize {
                        let src_start = y * rgba_layout.unpadded_bytes_per_row as usize;
                        let src_end = src_start + rgba_layout.unpadded_bytes_per_row as usize;
                        let dst_start = y * upload_bpr as usize;
                        staging[dst_start..dst_start + rgba_layout.unpadded_bytes_per_row as usize]
                            .copy_from_slice(&decompressed[src_start..src_end]);
                    }

                    self.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &tex.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: origin_y,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &staging,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(upload_bpr),
                            rows_per_image: Some(copy_height),
                        },
                        wgpu::Extent3d {
                            width: tex.width,
                            height: copy_height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
        }

        tex.dirty_ranges.clear();
        Ok(())
    }
}

fn coalesce_ranges(ranges: &mut Vec<Range<u64>>) {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
    for r in ranges.drain(..) {
        if r.start >= r.end {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        out.push(r);
    }
    *ranges = out;
}

fn coalesce_ranges_u32(ranges: &mut Vec<Range<u32>>) {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<u32>> = Vec::with_capacity(ranges.len());
    for r in ranges.drain(..) {
        if r.start >= r.end {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        out.push(r);
    }
    *ranges = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_x8_format_includes_srgb_variants() {
        assert!(is_x8_format(pci::AerogpuFormat::B8G8R8X8Unorm as u32));
        assert!(is_x8_format(pci::AerogpuFormat::R8G8B8X8Unorm as u32));
        assert!(is_x8_format(pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32));
        assert!(is_x8_format(pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32));

        assert!(!is_x8_format(pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32));
        assert!(!is_x8_format(pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32));
    }

    fn build_alloc_table_with_stride(entries: &[(u32, u64, u64)], entry_stride: u32) -> Vec<u8> {
        let size_bytes =
            ring::AerogpuAllocTableHeader::SIZE_BYTES as u32 + entries.len() as u32 * entry_stride;
        let mut bytes = vec![0u8; size_bytes as usize];

        bytes[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
        bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());
        bytes[12..16].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        bytes[16..20].copy_from_slice(&entry_stride.to_le_bytes());
        // reserved0 stays zeroed.

        for (i, (alloc_id, gpa, size_bytes)) in entries.iter().copied().enumerate() {
            let base = ring::AerogpuAllocTableHeader::SIZE_BYTES + i * entry_stride as usize;
            bytes[base..base + 4].copy_from_slice(&alloc_id.to_le_bytes());
            // flags = 0
            bytes[base + 8..base + 16].copy_from_slice(&gpa.to_le_bytes());
            bytes[base + 16..base + 24].copy_from_slice(&size_bytes.to_le_bytes());
            // reserved0 stays zeroed.
        }

        bytes
    }

    fn build_alloc_table(entries: &[(u32, u64, u64)]) -> Vec<u8> {
        build_alloc_table_with_stride(entries, ring::AerogpuAllocEntry::SIZE_BYTES as u32)
    }

    #[test]
    fn coalesce_ranges_merges_overlapping_and_adjacent() {
        let mut ranges = vec![10u64..12, 0..4, 4..8, 11..15, 20..20];
        coalesce_ranges(&mut ranges);
        assert_eq!(ranges, vec![0..8, 10..15]);
    }

    #[test]
    fn coalesce_ranges_u32_merges_overlapping_and_adjacent() {
        let mut ranges = vec![10u32..12, 0..4, 4..8, 11..15, 20..20];
        coalesce_ranges_u32(&mut ranges);
        assert_eq!(ranges, vec![0..8, 10..15]);
    }

    #[test]
    fn alloc_table_decode_accepts_valid_entries() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(1, 0x1000, 0x2000), (2, 0x3000, 0x4000)]);
        let table_gpa = 0x100u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let table =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap();
        assert_eq!(table.get(1).unwrap().gpa, 0x1000);
        assert_eq!(table.get(1).unwrap().size_bytes, 0x2000);
        assert_eq!(table.get(2).unwrap().gpa, 0x3000);
        assert_eq!(table.get(2).unwrap().size_bytes, 0x4000);
    }

    #[test]
    fn alloc_table_decode_accepts_extended_entry_stride_bytes() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let entry_stride = ring::AerogpuAllocEntry::SIZE_BYTES as u32 + 16;
        let table_bytes = build_alloc_table_with_stride(
            &[(1, 0x1000, 0x2000), (2, 0x3000, 0x4000)],
            entry_stride,
        );
        let table_gpa = 0x120u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let table =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap();
        assert_eq!(table.get(1).unwrap().gpa, 0x1000);
        assert_eq!(table.get(1).unwrap().size_bytes, 0x2000);
        assert_eq!(table.get(2).unwrap().gpa, 0x3000);
        assert_eq!(table.get(2).unwrap().size_bytes, 0x4000);
    }

    #[test]
    fn alloc_table_decode_accepts_zero_gpa() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(1, 0, 0x2000)]);
        let table_gpa = 0x180u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let table =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap();
        assert_eq!(table.get(1).unwrap().gpa, 0);
        assert_eq!(table.get(1).unwrap().size_bytes, 0x2000);
    }

    #[test]
    fn alloc_table_decode_rejects_alloc_id_zero() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(0, 0x1000, 0x2000)]);
        let table_gpa = 0x200u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let err =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap_err();
        match err {
            ExecutorError::Validation(message) => {
                assert!(message.contains("alloc_id must be non-zero"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn alloc_table_decode_rejects_duplicate_alloc_id() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(1, 0x1000, 0x2000), (1, 0x3000, 0x4000)]);
        let table_gpa = 0x300u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let err =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap_err();
        match err {
            ExecutorError::Validation(message) => {
                assert!(message.contains("duplicate"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn alloc_table_decode_rejects_size_bytes_too_small_for_layout() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let mut table_bytes = build_alloc_table(&[(1, 0x1000, 0x2000)]);
        // Corrupt the header size_bytes field so the prefix validation fails.
        table_bytes[8..12]
            .copy_from_slice(&(ring::AerogpuAllocTableHeader::SIZE_BYTES as u32).to_le_bytes());
        let table_gpa = 0x400u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let err =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap_err();
        match err {
            ExecutorError::Validation(message) => {
                assert!(message.contains("BadSizeField"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }
}
