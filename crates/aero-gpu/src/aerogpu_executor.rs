//! Host-side executor for the stable AeroGPU guest↔host command stream.
//!
//! The "real" executor is expected to translate the full AeroGPU IR into WebGPU
//! commands. For now we implement a minimal subset needed for validating
//! guest-memory-backed resources (`alloc_table` + `backing_alloc_id`) and
//! `RESOURCE_DIRTY_RANGE` uploads.

use std::collections::HashMap;
use std::ops::Range;

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_stream_header_le, AerogpuCmdDecodeError, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuCmdStreamIter, AEROGPU_CLEAR_COLOR as CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER as USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL as USAGE_DEPTH_STENCIL,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER as USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET as USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE as USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER as USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{parse_and_validate_abi_version_u32, AerogpuAbiError, AerogpuFormat};
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuAllocEntry as ProtocolAllocEntry, AerogpuAllocTableHeader as ProtocolAllocTableHeader,
    AEROGPU_ALLOC_TABLE_MAGIC,
};

use crate::guest_memory::{GuestMemory, GuestMemoryError};

// Selected opcodes from `drivers/aerogpu/protocol/aerogpu_cmd.h`.
const OP_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OP_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OP_DESTROY_RESOURCE: u32 = AerogpuCmdOpcode::DestroyResource as u32;
const OP_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
const OP_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;

const OP_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OP_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
const OP_SET_INDEX_BUFFER: u32 = AerogpuCmdOpcode::SetIndexBuffer as u32;
const OP_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;

const OP_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OP_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
const OP_DRAW_INDEXED: u32 = AerogpuCmdOpcode::DrawIndexed as u32;

// `enum aerogpu_format` from `aerogpu_pci.h`.
const FMT_B8G8R8A8_UNORM: u32 = AerogpuFormat::B8G8R8A8Unorm as u32;
const FMT_B8G8R8X8_UNORM: u32 = AerogpuFormat::B8G8R8X8Unorm as u32;
const FMT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
const FMT_R8G8B8X8_UNORM: u32 = AerogpuFormat::R8G8B8X8Unorm as u32;

const STREAM_HEADER_SIZE: usize = ProtocolCmdStreamHeader::SIZE_BYTES;
const ALLOC_TABLE_HEADER_SIZE: usize = ProtocolAllocTableHeader::SIZE_BYTES;
const ALLOC_ENTRY_SIZE: usize = ProtocolAllocEntry::SIZE_BYTES;

const ALLOC_TABLE_MAGIC_OFFSET: usize = core::mem::offset_of!(ProtocolAllocTableHeader, magic);
const ALLOC_TABLE_ABI_VERSION_OFFSET: usize =
    core::mem::offset_of!(ProtocolAllocTableHeader, abi_version);
const ALLOC_TABLE_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolAllocTableHeader, size_bytes);
const ALLOC_TABLE_ENTRY_COUNT_OFFSET: usize =
    core::mem::offset_of!(ProtocolAllocTableHeader, entry_count);
const ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolAllocTableHeader, entry_stride_bytes);

const ALLOC_ENTRY_ALLOC_ID_OFFSET: usize = core::mem::offset_of!(ProtocolAllocEntry, alloc_id);
const ALLOC_ENTRY_GPA_OFFSET: usize = core::mem::offset_of!(ProtocolAllocEntry, gpa);
const ALLOC_ENTRY_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolAllocEntry, size_bytes);

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, ExecutorError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(ExecutorError::TruncatedPacket)?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64, ExecutorError> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or(ExecutorError::TruncatedPacket)?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn read_i32_le(bytes: &[u8], offset: usize) -> Result<i32, ExecutorError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(ExecutorError::TruncatedPacket)?;
    Ok(i32::from_le_bytes(slice.try_into().unwrap()))
}

fn align_to(value: u32, alignment: u32) -> u32 {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
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
    pub gpa: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Default, Clone)]
pub struct AllocTable {
    entries: HashMap<u32, AllocEntry>,
}

impl AllocTable {
    pub fn new(entries: impl IntoIterator<Item = (u32, AllocEntry)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    pub fn get(&self, alloc_id: u32) -> Option<&AllocEntry> {
        self.entries.get(&alloc_id)
    }

    pub fn decode_from_guest_memory(
        guest_memory: &dyn GuestMemory,
        table_gpa: u64,
        table_size_bytes: u32,
    ) -> Result<Self, ExecutorError> {
        if table_gpa == 0 || table_size_bytes == 0 {
            return Err(ExecutorError::Validation(
                "alloc table gpa/size must be non-zero".into(),
            ));
        }

        let table_size = table_size_bytes as usize;
        if table_size < ALLOC_TABLE_HEADER_SIZE {
            return Err(ExecutorError::Validation(format!(
                "alloc table size_bytes too small (got {table_size_bytes}, need {ALLOC_TABLE_HEADER_SIZE})"
            )));
        }

        let mut header = [0u8; ALLOC_TABLE_HEADER_SIZE];
        guest_memory.read(table_gpa, &mut header)?;

        let magic = read_u32_le(&header, ALLOC_TABLE_MAGIC_OFFSET)?;
        if magic != AEROGPU_ALLOC_TABLE_MAGIC {
            return Err(ExecutorError::Validation(format!(
                "invalid alloc table magic 0x{magic:08x}"
            )));
        }

        let abi_version = read_u32_le(&header, ALLOC_TABLE_ABI_VERSION_OFFSET)?;
        match parse_and_validate_abi_version_u32(abi_version) {
            Ok(_) => {}
            Err(AerogpuAbiError::UnsupportedMajor { found }) => {
                return Err(ExecutorError::Validation(format!(
                    "unsupported alloc table ABI major version {found}"
                )));
            }
        }

        let size_bytes = read_u32_le(&header, ALLOC_TABLE_SIZE_BYTES_OFFSET)?;
        let size_usize = size_bytes as usize;
        if size_usize < ALLOC_TABLE_HEADER_SIZE || size_usize > table_size {
            return Err(ExecutorError::Validation(format!(
                "invalid alloc table header size_bytes={size_bytes} (provided buffer size={table_size_bytes})"
            )));
        }

        let entry_count = read_u32_le(&header, ALLOC_TABLE_ENTRY_COUNT_OFFSET)?;
        let entry_stride_bytes = read_u32_le(&header, ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET)?;
        if entry_stride_bytes < ALLOC_ENTRY_SIZE as u32 {
            return Err(ExecutorError::Validation(format!(
                "alloc table entry_stride_bytes={entry_stride_bytes} too small (min {ALLOC_ENTRY_SIZE})"
            )));
        }

        let required = ALLOC_TABLE_HEADER_SIZE as u64
            + (entry_count as u64).saturating_mul(entry_stride_bytes as u64);
        if required > size_bytes as u64 {
            return Err(ExecutorError::Validation(format!(
                "alloc table requires {required} bytes but header size_bytes={size_bytes}"
            )));
        }

        let mut table = AllocTable::default();
        for i in 0..entry_count {
            let entry_gpa = table_gpa
                + ALLOC_TABLE_HEADER_SIZE as u64
                + (i as u64) * (entry_stride_bytes as u64);
            let mut entry_bytes = [0u8; ALLOC_ENTRY_SIZE];
            guest_memory.read(entry_gpa, &mut entry_bytes)?;

            let alloc_id = read_u32_le(&entry_bytes, ALLOC_ENTRY_ALLOC_ID_OFFSET)?;
            if alloc_id == 0 {
                return Err(ExecutorError::Validation(
                    "alloc table entry alloc_id must be non-zero".into(),
                ));
            }
            if table.entries.contains_key(&alloc_id) {
                return Err(ExecutorError::Validation(format!(
                    "alloc table contains duplicate alloc_id={alloc_id}"
                )));
            }

            let gpa = read_u64_le(&entry_bytes, ALLOC_ENTRY_GPA_OFFSET)?;
            let size_bytes = read_u64_le(&entry_bytes, ALLOC_ENTRY_SIZE_BYTES_OFFSET)?;

            table
                .entries
                .insert(alloc_id, AllocEntry { gpa, size_bytes });
        }

        Ok(table)
    }
}

#[derive(Debug, Clone, Copy)]
struct GuestBufferBacking {
    base_gpa: u64,
}

#[derive(Debug, Clone, Copy)]
struct GuestTextureBacking {
    base_gpa: u64,
    row_pitch_bytes: u32,
    size_bytes: u64,
}

#[derive(Debug)]
struct BufferResource {
    buffer: wgpu::Buffer,
    size_bytes: u64,
    backing: Option<GuestBufferBacking>,
    dirty_ranges: Vec<Range<u64>>,
}

#[derive(Debug)]
struct TextureResource {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    bytes_per_pixel: u32,
    linear_row_pitch_bytes: u32,
    backing: Option<GuestTextureBacking>,
    dirty_ranges: Vec<Range<u64>>,
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
            wgpu::TextureFormat::Bgra8Unorm,
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
        guest_memory: &dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> ExecutionReport {
        match self.execute_cmd_stream_internal(bytes, guest_memory, alloc_table) {
            Ok(packets_processed) => ExecutionReport {
                packets_processed,
                events: Vec::new(),
            },
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
        guest_memory: &dyn GuestMemory,
        cmd_gpa: u64,
        cmd_size_bytes: u32,
        alloc_table_gpa: u64,
        alloc_table_size_bytes: u32,
    ) -> ExecutionReport {
        let cmd_size = cmd_size_bytes as usize;
        let mut cmd_bytes = vec![0u8; cmd_size];
        if let Err(err) = guest_memory.read(cmd_gpa, &mut cmd_bytes) {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("failed to read command stream: {err}"),
                }],
            };
        }

        let alloc_table = if alloc_table_gpa != 0 && alloc_table_size_bytes != 0 {
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
        } else {
            None
        };

        match alloc_table.as_ref() {
            Some(table) => self.process_cmd_stream(&cmd_bytes, guest_memory, Some(table)),
            None => self.process_cmd_stream(&cmd_bytes, guest_memory, None),
        }
    }

    pub fn execute_cmd_stream(
        &mut self,
        bytes: &[u8],
        guest_memory: &dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        self.execute_cmd_stream_internal(bytes, guest_memory, alloc_table)
            .map(|_| ())
            .map_err(|(_, err, _)| err)
    }

    fn execute_cmd_stream_internal(
        &mut self,
        bytes: &[u8],
        guest_memory: &dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<u32, (usize, ExecutorError, u32)> {
        let mut packets_processed = 0u32;

        let header = match decode_cmd_stream_header_le(bytes) {
            Ok(header) => header,
            Err(err) => {
                let mapped = match err {
                    AerogpuCmdDecodeError::BufferTooSmall => ExecutorError::TruncatedStream,
                    AerogpuCmdDecodeError::BadMagic { found } => ExecutorError::BadStreamMagic(found),
                    AerogpuCmdDecodeError::Abi(AerogpuAbiError::UnsupportedMajor { found }) => {
                        ExecutorError::Validation(format!("unsupported ABI major version {found}"))
                    }
                    AerogpuCmdDecodeError::BadSizeBytes { found } => ExecutorError::BadStreamSize {
                        size_bytes: found,
                        buffer_len: bytes.len(),
                    },
                    other => ExecutorError::Validation(format!("command stream header decode error: {other:?}")),
                };
                return Err((0, mapped, packets_processed));
            }
        };

        let size_bytes_usize = header.size_bytes as usize;
        if size_bytes_usize < STREAM_HEADER_SIZE || size_bytes_usize > bytes.len() {
            return Err((
                0,
                ExecutorError::BadStreamSize {
                    size_bytes: header.size_bytes,
                    buffer_len: bytes.len(),
                },
                packets_processed,
            ));
        }

        let stream_bytes = &bytes[..size_bytes_usize];
        let iter = match AerogpuCmdStreamIter::new(stream_bytes) {
            Ok(iter) => iter,
            Err(err) => {
                return Err((
                    0,
                    ExecutorError::Validation(format!(
                        "failed to create command stream iterator: {err:?}"
                    )),
                    packets_processed,
                ))
            }
        };

        let mut offset = STREAM_HEADER_SIZE;
        for packet in iter {
            let packet = match packet {
                Ok(packet) => packet,
                Err(err) => {
                    let mapped = match err {
                        AerogpuCmdDecodeError::BufferTooSmall
                        | AerogpuCmdDecodeError::PacketOverrunsStream { .. } => ExecutorError::TruncatedStream,
                        AerogpuCmdDecodeError::BadSizeBytes { found } => ExecutorError::InvalidPacketSize(found),
                        AerogpuCmdDecodeError::SizeNotAligned { found } => ExecutorError::MisalignedPacketSize(found),
                        other => ExecutorError::Validation(format!("packet decode error: {other:?}")),
                    };
                    return Err((offset, mapped, packets_processed));
                }
            };

            let cmd_size = packet.hdr.size_bytes as usize;
            let end = match offset.checked_add(cmd_size) {
                Some(end) => end,
                None => {
                    return Err((
                        offset,
                        ExecutorError::Validation("packet size overflow".into()),
                        packets_processed,
                    ))
                }
            };

            let cmd_bytes = match stream_bytes.get(offset..end) {
                Some(cmd_bytes) => cmd_bytes,
                None => return Err((offset, ExecutorError::TruncatedStream, packets_processed)),
            };

            self.exec_packet(packet.hdr.opcode, cmd_bytes, guest_memory, alloc_table)
                .map_err(|e| (offset, e, packets_processed))?;

            packets_processed += 1;
            offset = end;
        }

        Ok(packets_processed)
    }

    fn exec_packet(
        &mut self,
        opcode: u32,
        cmd_bytes: &[u8],
        guest_memory: &dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        match opcode {
            OP_CREATE_BUFFER => self.exec_create_buffer(cmd_bytes, alloc_table),
            OP_CREATE_TEXTURE2D => self.exec_create_texture2d(cmd_bytes, alloc_table),
            OP_DESTROY_RESOURCE => self.exec_destroy_resource(cmd_bytes),
            OP_RESOURCE_DIRTY_RANGE => self.exec_resource_dirty_range(cmd_bytes),
            OP_UPLOAD_RESOURCE => self.exec_upload_resource(cmd_bytes),

            OP_SET_RENDER_TARGETS => self.exec_set_render_targets(cmd_bytes),
            OP_SET_VERTEX_BUFFERS => self.exec_set_vertex_buffers(cmd_bytes),
            OP_SET_INDEX_BUFFER => self.exec_set_index_buffer(cmd_bytes),
            OP_SET_TEXTURE => self.exec_set_texture(cmd_bytes),

            OP_CLEAR => self.exec_clear(cmd_bytes),
            OP_DRAW => self.exec_draw(cmd_bytes, guest_memory),
            OP_DRAW_INDEXED => self.exec_draw_indexed(cmd_bytes, guest_memory),

            _ => Ok(()), // unknown/unsupported opcode
        }
    }

    fn exec_create_buffer(
        &mut self,
        cmd: &[u8],
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        if cmd.len() < 40 {
            return Err(ExecutorError::TruncatedPacket);
        }

        let buffer_handle = read_u32_le(cmd, 8)?;
        let usage_flags = read_u32_le(cmd, 12)?;
        let size_bytes = read_u64_le(cmd, 16)?;
        let backing_alloc_id = read_u32_le(cmd, 24)?;
        let backing_offset_bytes = read_u32_le(cmd, 28)?;

        if self.buffers.contains_key(&buffer_handle) || self.textures.contains_key(&buffer_handle) {
            return Err(ExecutorError::Validation(format!(
                "resource handle {buffer_handle} already exists"
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
                base_gpa: entry.gpa + backing_offset,
            })
        };

        let mut wgpu_usage = wgpu::BufferUsages::COPY_DST;
        if (usage_flags & USAGE_VERTEX_BUFFER) != 0 {
            wgpu_usage |= wgpu::BufferUsages::VERTEX;
        }
        if (usage_flags & USAGE_INDEX_BUFFER) != 0 {
            wgpu_usage |= wgpu::BufferUsages::INDEX;
        }
        if (usage_flags & USAGE_CONSTANT_BUFFER) != 0 {
            wgpu_usage |= wgpu::BufferUsages::UNIFORM;
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
                backing,
                dirty_ranges: Vec::new(),
            },
        );
        Ok(())
    }

    fn map_format(format: u32) -> Result<(wgpu::TextureFormat, u32), ExecutorError> {
        let (fmt, bpp) = match format {
            FMT_B8G8R8A8_UNORM | FMT_B8G8R8X8_UNORM => (wgpu::TextureFormat::Bgra8Unorm, 4),
            FMT_R8G8B8A8_UNORM | FMT_R8G8B8X8_UNORM => (wgpu::TextureFormat::Rgba8Unorm, 4),
            _ => {
                return Err(ExecutorError::Validation(format!(
                    "unsupported aerogpu_format={format}"
                )))
            }
        };
        Ok((fmt, bpp))
    }

    fn exec_create_texture2d(
        &mut self,
        cmd: &[u8],
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        if cmd.len() < 56 {
            return Err(ExecutorError::TruncatedPacket);
        }

        let texture_handle = read_u32_le(cmd, 8)?;
        let usage_flags = read_u32_le(cmd, 12)?;
        let format = read_u32_le(cmd, 16)?;
        let width = read_u32_le(cmd, 20)?;
        let height = read_u32_le(cmd, 24)?;
        let mip_levels = read_u32_le(cmd, 28)?;
        let array_layers = read_u32_le(cmd, 32)?;
        let row_pitch_bytes = read_u32_le(cmd, 36)?;
        let backing_alloc_id = read_u32_le(cmd, 40)?;
        let backing_offset_bytes = read_u32_le(cmd, 44)?;

        if self.buffers.contains_key(&texture_handle) || self.textures.contains_key(&texture_handle)
        {
            return Err(ExecutorError::Validation(format!(
                "resource handle {texture_handle} already exists"
            )));
        }

        if mip_levels != 1 || array_layers != 1 {
            return Err(ExecutorError::Validation(format!(
                "only mip_levels=1 and array_layers=1 are supported for now (got mip_levels={mip_levels}, array_layers={array_layers})"
            )));
        }

        let (wgpu_format, bytes_per_pixel) = Self::map_format(format)?;

        let min_row_bytes = width
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| ExecutorError::Validation("texture row size overflow".into()))?;
        let linear_row_pitch_bytes = if row_pitch_bytes != 0 {
            if row_pitch_bytes < min_row_bytes {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D row_pitch_bytes={row_pitch_bytes} smaller than minimum row size {min_row_bytes}"
                )));
            }
            row_pitch_bytes
        } else {
            min_row_bytes
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
                .checked_mul(u64::from(height))
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
                base_gpa: entry.gpa + backing_offset,
                row_pitch_bytes,
                size_bytes: required_bytes,
            })
        };

        let mut usage = wgpu::TextureUsages::empty();
        if (usage_flags & USAGE_TEXTURE) != 0 {
            usage |= wgpu::TextureUsages::TEXTURE_BINDING;
        }
        if (usage_flags & (USAGE_RENDER_TARGET | USAGE_DEPTH_STENCIL)) != 0 {
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
                width,
                height,
                format: wgpu_format,
                bytes_per_pixel,
                linear_row_pitch_bytes,
                backing,
                dirty_ranges: Vec::new(),
            },
        );
        Ok(())
    }

    fn exec_destroy_resource(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 16 {
            return Err(ExecutorError::TruncatedPacket);
        }
        let handle = read_u32_le(cmd, 8)?;
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

    fn exec_resource_dirty_range(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 32 {
            return Err(ExecutorError::TruncatedPacket);
        }
        let handle = read_u32_le(cmd, 8)?;
        let offset_bytes = read_u64_le(cmd, 16)?;
        let size_bytes = read_u64_le(cmd, 24)?;
        if size_bytes == 0 {
            return Ok(());
        }

        if let Some(buffer) = self.buffers.get_mut(&handle) {
            if buffer.backing.is_none() {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE on host-owned buffer {handle}"
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
            buffer.dirty_ranges.push(offset_bytes..end);
            coalesce_ranges(&mut buffer.dirty_ranges);
            return Ok(());
        }

        if let Some(tex) = self.textures.get_mut(&handle) {
            let Some(backing) = tex.backing else {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE on host-owned texture {handle}"
                )));
            };
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

    fn exec_upload_resource(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 32 {
            return Err(ExecutorError::TruncatedPacket);
        }

        let handle = read_u32_le(cmd, 8)?;
        let offset_bytes = read_u64_le(cmd, 16)?;
        let size_bytes = read_u64_le(cmd, 24)?;
        if size_bytes == 0 {
            return Ok(());
        }

        let data_len = usize::try_from(size_bytes).map_err(|_| {
            ExecutorError::Validation("UPLOAD_RESOURCE size_bytes too large".into())
        })?;
        let data_end = 32usize
            .checked_add(data_len)
            .ok_or_else(|| ExecutorError::Validation("UPLOAD_RESOURCE size overflow".into()))?;
        if cmd.len() < data_end {
            return Err(ExecutorError::TruncatedPacket);
        }
        let data = &cmd[32..data_end];

        if let Some(buffer) = self.buffers.get_mut(&handle) {
            if buffer.backing.is_some() {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE on guest-backed buffer {handle} is not supported (use RESOURCE_DIRTY_RANGE)"
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

            let row_pitch = u64::from(tex.linear_row_pitch_bytes);
            if row_pitch == 0 {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE texture {handle} is missing row_pitch_bytes"
                )));
            }

            if offset_bytes % row_pitch != 0 || size_bytes % row_pitch != 0 {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE for texture {handle} must be row-aligned (offset_bytes and size_bytes must be multiples of row_pitch_bytes={})",
                    tex.linear_row_pitch_bytes
                )));
            }

            let start_row = (offset_bytes / row_pitch) as u32;
            let row_count = (size_bytes / row_pitch) as u32;
            let end_row = start_row.saturating_add(row_count);
            if end_row > tex.height {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE out of bounds for texture {handle} (rows {start_row}..{end_row}, height={})",
                    tex.height
                )));
            }

            let unpadded_bpr = tex
                .width
                .checked_mul(tex.bytes_per_pixel)
                .ok_or_else(|| ExecutorError::Validation("texture row size overflow".into()))?;

            if tex.linear_row_pitch_bytes < unpadded_bpr {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE texture row_pitch_bytes={} smaller than minimum row size {unpadded_bpr}",
                    tex.linear_row_pitch_bytes
                )));
            }

            let upload_bpr = if tex.linear_row_pitch_bytes % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT == 0
            {
                tex.linear_row_pitch_bytes
            } else {
                align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            };

            let mut repacked = Vec::<u8>::new();
            let bytes: &[u8] = if upload_bpr == tex.linear_row_pitch_bytes {
                data
            } else {
                // Repack to satisfy WebGPU 256-byte row alignment while ignoring any row padding.
                repacked.resize(upload_bpr as usize * row_count as usize, 0);
                for row in 0..row_count as usize {
                    let src_start = row * tex.linear_row_pitch_bytes as usize;
                    let src_end = src_start + unpadded_bpr as usize;
                    let dst_start = row * upload_bpr as usize;
                    repacked[dst_start..dst_start + unpadded_bpr as usize]
                        .copy_from_slice(&data[src_start..src_end]);
                }
                &repacked
            };

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &tex.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: start_row,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(upload_bpr),
                    rows_per_image: Some(row_count),
                },
                wgpu::Extent3d {
                    width: tex.width,
                    height: row_count,
                    depth_or_array_layers: 1,
                },
            );
            return Ok(());
        }

        Err(ExecutorError::Validation(format!(
            "UPLOAD_RESOURCE for unknown resource {handle}"
        )))
    }

    fn exec_set_render_targets(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 48 {
            return Err(ExecutorError::TruncatedPacket);
        }
        let color_count = read_u32_le(cmd, 8)?;
        if color_count > 1 {
            return Err(ExecutorError::Validation(
                "only color_count<=1 is supported".into(),
            ));
        }
        let color0 = read_u32_le(cmd, 16)?;
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

    fn exec_set_vertex_buffers(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 16 {
            return Err(ExecutorError::TruncatedPacket);
        }
        let start_slot = read_u32_le(cmd, 8)?;
        let buffer_count = read_u32_le(cmd, 12)?;
        if start_slot != 0 {
            return Err(ExecutorError::Validation(
                "only start_slot=0 is supported".into(),
            ));
        }
        if buffer_count == 0 {
            self.state.vertex_buffer = None;
            return Ok(());
        }

        let expected_size = 16usize
            .checked_add(buffer_count as usize * 16)
            .ok_or_else(|| {
                ExecutorError::Validation("vertex buffer binding size overflow".into())
            })?;
        if cmd.len() < expected_size {
            return Err(ExecutorError::TruncatedPacket);
        }

        // Only track slot 0 for now.
        let binding_off = 16;
        let buffer = read_u32_le(cmd, binding_off + 0)?;
        let stride_bytes = read_u32_le(cmd, binding_off + 4)?;
        let offset_bytes = read_u32_le(cmd, binding_off + 8)?;

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

    fn exec_set_index_buffer(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 24 {
            return Err(ExecutorError::TruncatedPacket);
        }
        let buffer = read_u32_le(cmd, 8)?;
        let format_raw = read_u32_le(cmd, 12)?;
        let offset_bytes = read_u32_le(cmd, 16)?;

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
            0 => wgpu::IndexFormat::Uint16,
            1 => wgpu::IndexFormat::Uint32,
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
        if (offset_bytes as u64) % align != 0 {
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

    fn exec_set_texture(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 24 {
            return Err(ExecutorError::TruncatedPacket);
        }
        let slot = read_u32_le(cmd, 12)?;
        if slot != 0 {
            return Err(ExecutorError::Validation(
                "only texture slot 0 is supported".into(),
            ));
        }
        let texture = read_u32_le(cmd, 16)?;
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

    fn exec_clear(&mut self, cmd: &[u8]) -> Result<(), ExecutorError> {
        if cmd.len() < 36 {
            return Err(ExecutorError::TruncatedPacket);
        }
        let flags = read_u32_le(cmd, 8)?;
        if flags & CLEAR_COLOR == 0 {
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

        let r = f32::from_bits(read_u32_le(cmd, 12)?);
        let g = f32::from_bits(read_u32_le(cmd, 16)?);
        let b = f32::from_bits(read_u32_le(cmd, 20)?);
        let a = f32::from_bits(read_u32_le(cmd, 24)?);

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
        cmd: &[u8],
        guest_memory: &dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        if cmd.len() < 24 {
            return Err(ExecutorError::TruncatedPacket);
        }

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
        self.flush_texture_if_dirty(rt, guest_memory)?;
        self.flush_buffer_if_dirty(vb.buffer, guest_memory)?;
        self.flush_texture_if_dirty(tex0, guest_memory)?;

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

        let vertex_count = read_u32_le(cmd, 8)?;
        let instance_count = read_u32_le(cmd, 12)?;
        let first_vertex = read_u32_le(cmd, 16)?;
        let first_instance = read_u32_le(cmd, 20)?;

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

    fn exec_draw_indexed(
        &mut self,
        cmd: &[u8],
        guest_memory: &dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        if cmd.len() < 28 {
            return Err(ExecutorError::TruncatedPacket);
        }

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

        self.flush_texture_if_dirty(rt, guest_memory)?;
        self.flush_buffer_if_dirty(vb.buffer, guest_memory)?;
        self.flush_buffer_if_dirty(ib.buffer, guest_memory)?;
        self.flush_texture_if_dirty(tex0, guest_memory)?;

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

        let index_count = read_u32_le(cmd, 8)?;
        let instance_count = read_u32_le(cmd, 12)?;
        let first_index = read_u32_le(cmd, 16)?;
        let base_vertex = read_i32_le(cmd, 20)?;
        let first_instance = read_u32_le(cmd, 24)?;

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
        guest_memory: &dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        let Some(buffer) = self.buffers.get_mut(&handle) else {
            return Err(ExecutorError::Validation(format!(
                "unknown buffer {handle}"
            )));
        };
        let Some(backing) = buffer.backing else {
            // Host-owned buffers are updated through UPLOAD_RESOURCE (not implemented yet).
            return Ok(());
        };
        if buffer.dirty_ranges.is_empty() {
            return Ok(());
        }

        for range in &buffer.dirty_ranges {
            let mut data = vec![0u8; (range.end - range.start) as usize];
            guest_memory.read(backing.base_gpa + range.start, &mut data)?;
            self.queue.write_buffer(&buffer.buffer, range.start, &data);
        }

        buffer.dirty_ranges.clear();
        Ok(())
    }

    fn flush_texture_if_dirty(
        &mut self,
        handle: u32,
        guest_memory: &dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        let Some(tex) = self.textures.get_mut(&handle) else {
            return Err(ExecutorError::Validation(format!(
                "unknown texture {handle}"
            )));
        };
        let Some(backing) = tex.backing else {
            // Host-owned textures are updated through UPLOAD_RESOURCE (not implemented yet).
            return Ok(());
        };
        if tex.dirty_ranges.is_empty() {
            return Ok(());
        }

        let row_pitch = backing.row_pitch_bytes as u64;
        let mut row_ranges = Vec::<Range<u32>>::new();
        for r in &tex.dirty_ranges {
            let start_row = (r.start / row_pitch) as u32;
            let end_row = ((r.end + row_pitch - 1) / row_pitch) as u32;
            row_ranges.push(start_row..end_row);
        }
        coalesce_ranges_u32(&mut row_ranges);
        for rows in &row_ranges {
            if rows.end > tex.height {
                return Err(ExecutorError::Validation(format!(
                    "computed dirty row range {rows:?} exceeds texture height {}",
                    tex.height
                )));
            }
        }

        let unpadded_bpr = tex
            .width
            .checked_mul(tex.bytes_per_pixel)
            .ok_or_else(|| ExecutorError::Validation("texture row size overflow".into()))?;
        let upload_bpr = if backing.row_pitch_bytes % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT == 0 {
            backing.row_pitch_bytes
        } else {
            align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        };

        for rows in row_ranges {
            let height = rows.end.saturating_sub(rows.start);
            if height == 0 {
                continue;
            }

            let mut staging = vec![0u8; upload_bpr as usize * height as usize];
            for i in 0..height {
                let row = rows.start + i;
                let src_gpa = backing.base_gpa + (row as u64) * row_pitch;
                let dst_off = i as usize * upload_bpr as usize;
                guest_memory.read(
                    src_gpa,
                    &mut staging[dst_off..dst_off + unpadded_bpr as usize],
                )?;
            }

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &tex.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: rows.start,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &staging,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(upload_bpr),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width: tex.width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
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
}
