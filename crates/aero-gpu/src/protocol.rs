//! AeroGPU Guest↔Host command stream protocol (host-side parser).
//!
//! This module mirrors the C ABI defined in `drivers/aerogpu/protocol/aerogpu_cmd.h`.
//! The host consumes a byte slice containing:
//! - `aerogpu_cmd_stream_header`
//! - a sequence of command packets, each starting with `aerogpu_cmd_hdr`
//!
//! The parser is intentionally conservative:
//! - validates sizes and alignment
//! - skips unknown opcodes using `size_bytes`
//! - never performs unaligned reads into `repr(C)` structs
//!
//! This allows the protocol to be consumed safely from guest-provided memory.

use core::fmt;

pub const AEROGPU_CMD_STREAM_MAGIC: u32 = 0x444D_4341; // "ACMD" little-endian

pub const AEROGPU_INPUT_LAYOUT_BLOB_MAGIC: u32 = 0x5941_4C49; // "ILAY" little-endian
pub const AEROGPU_INPUT_LAYOUT_BLOB_VERSION: u32 = 1;

pub const AEROGPU_MAX_RENDER_TARGETS: usize = 8;

const STREAM_HEADER_SIZE: usize = 24;
const CMD_HDR_SIZE: usize = 8;
const VERTEX_BUFFER_BINDING_SIZE: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuCmdStreamHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub flags: u32,
    pub reserved0: u32,
    pub reserved1: u32,
}

impl AeroGpuCmdStreamHeader {
    pub fn is_magic_valid(&self) -> bool {
        self.magic == AEROGPU_CMD_STREAM_MAGIC
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuCmdHdr {
    pub opcode: u32,
    pub size_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AeroGpuOpcode {
    Nop = 0,
    DebugMarker = 1,

    // Presentation
    Present = 0x700,
    PresentEx = 0x701,

    // D3D9Ex/DWM shared surface interop.
    ExportSharedSurface = 0x710,
    ImportSharedSurface = 0x711,

    // Explicit flush.
    Flush = 0x720,

    // Resource / memory
    CreateBuffer = 0x100,
    CreateTexture2d = 0x101,
    DestroyResource = 0x102,
    ResourceDirtyRange = 0x103,
    UploadResource = 0x104,

    // Shaders
    CreateShaderDxbc = 0x200,
    DestroyShader = 0x201,
    BindShaders = 0x202,
    SetShaderConstantsF = 0x203,

    // Input layouts
    CreateInputLayout = 0x204,
    DestroyInputLayout = 0x205,
    SetInputLayout = 0x206,

    // Pipeline state
    SetBlendState = 0x300,
    SetDepthStencilState = 0x301,
    SetRasterizerState = 0x302,

    // Render targets + dynamic state
    SetRenderTargets = 0x400,
    SetViewport = 0x401,
    SetScissor = 0x402,

    // Input assembler
    SetVertexBuffers = 0x500,
    SetIndexBuffer = 0x501,
    SetPrimitiveTopology = 0x502,

    // Resource binding / state
    SetTexture = 0x510,
    SetSamplerState = 0x511,
    SetRenderState = 0x512,

    // Drawing
    Clear = 0x600,
    Draw = 0x601,
    DrawIndexed = 0x602,
}

impl AeroGpuOpcode {
    fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            x if x == Self::Nop as u32 => Self::Nop,
            x if x == Self::DebugMarker as u32 => Self::DebugMarker,

            x if x == Self::CreateBuffer as u32 => Self::CreateBuffer,
            x if x == Self::CreateTexture2d as u32 => Self::CreateTexture2d,
            x if x == Self::DestroyResource as u32 => Self::DestroyResource,
            x if x == Self::ResourceDirtyRange as u32 => Self::ResourceDirtyRange,
            x if x == Self::UploadResource as u32 => Self::UploadResource,

            x if x == Self::CreateShaderDxbc as u32 => Self::CreateShaderDxbc,
            x if x == Self::DestroyShader as u32 => Self::DestroyShader,
            x if x == Self::BindShaders as u32 => Self::BindShaders,
            x if x == Self::SetShaderConstantsF as u32 => Self::SetShaderConstantsF,

            x if x == Self::CreateInputLayout as u32 => Self::CreateInputLayout,
            x if x == Self::DestroyInputLayout as u32 => Self::DestroyInputLayout,
            x if x == Self::SetInputLayout as u32 => Self::SetInputLayout,

            x if x == Self::SetBlendState as u32 => Self::SetBlendState,
            x if x == Self::SetDepthStencilState as u32 => Self::SetDepthStencilState,
            x if x == Self::SetRasterizerState as u32 => Self::SetRasterizerState,

            x if x == Self::SetRenderTargets as u32 => Self::SetRenderTargets,
            x if x == Self::SetViewport as u32 => Self::SetViewport,
            x if x == Self::SetScissor as u32 => Self::SetScissor,

            x if x == Self::SetVertexBuffers as u32 => Self::SetVertexBuffers,
            x if x == Self::SetIndexBuffer as u32 => Self::SetIndexBuffer,
            x if x == Self::SetPrimitiveTopology as u32 => Self::SetPrimitiveTopology,

            x if x == Self::SetTexture as u32 => Self::SetTexture,
            x if x == Self::SetSamplerState as u32 => Self::SetSamplerState,
            x if x == Self::SetRenderState as u32 => Self::SetRenderState,

            x if x == Self::Clear as u32 => Self::Clear,
            x if x == Self::Draw as u32 => Self::Draw,
            x if x == Self::DrawIndexed as u32 => Self::DrawIndexed,

            x if x == Self::Present as u32 => Self::Present,
            x if x == Self::PresentEx as u32 => Self::PresentEx,

            x if x == Self::ExportSharedSurface as u32 => Self::ExportSharedSurface,
            x if x == Self::ImportSharedSurface as u32 => Self::ImportSharedSurface,

            x if x == Self::Flush as u32 => Self::Flush,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuBlendState {
    pub enable: u32,
    pub src_factor: u32,
    pub dst_factor: u32,
    pub blend_op: u32,
    pub color_write_mask: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuDepthStencilState {
    pub depth_enable: u32,
    pub depth_write_enable: u32,
    pub depth_func: u32,
    pub stencil_enable: u32,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuRasterizerState {
    pub fill_mode: u32,
    pub cull_mode: u32,
    pub front_ccw: u32,
    pub scissor_enable: u32,
    pub depth_bias: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuInputLayoutBlobHeader {
    pub magic: u32,
    pub version: u32,
    pub element_count: u32,
    pub reserved0: u32,
}

impl AeroGpuInputLayoutBlobHeader {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 16 {
            return None;
        }
        Some(Self {
            magic: read_u32_le(&bytes[0..4]),
            version: read_u32_le(&bytes[4..8]),
            element_count: read_u32_le(&bytes[8..12]),
            reserved0: read_u32_le(&bytes[12..16]),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuInputLayoutElementDxgi {
    pub semantic_name_hash: u32,
    pub semantic_index: u32,
    pub dxgi_format: u32,
    pub input_slot: u32,
    pub aligned_byte_offset: u32,
    pub input_slot_class: u32,
    pub instance_data_step_rate: u32,
}

impl AeroGpuInputLayoutElementDxgi {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 28 {
            return None;
        }
        Some(Self {
            semantic_name_hash: read_u32_le(&bytes[0..4]),
            semantic_index: read_u32_le(&bytes[4..8]),
            dxgi_format: read_u32_le(&bytes[8..12]),
            input_slot: read_u32_le(&bytes[12..16]),
            aligned_byte_offset: read_u32_le(&bytes[16..20]),
            input_slot_class: read_u32_le(&bytes[20..24]),
            instance_data_step_rate: read_u32_le(&bytes[24..28]),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AeroGpuCmd<'a> {
    Nop,
    DebugMarker {
        bytes: &'a [u8],
    },

    Present {
        scanout_id: u32,
        flags: u32,
    },
    PresentEx {
        scanout_id: u32,
        flags: u32,
        d3d9_present_flags: u32,
    },

    ExportSharedSurface {
        resource_handle: u32,
        share_token: u64,
    },
    ImportSharedSurface {
        out_resource_handle: u32,
        share_token: u64,
    },

    Flush,

    // Resource / memory
    CreateBuffer {
        buffer_handle: u32,
        usage_flags: u32,
        size_bytes: u64,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
    },
    CreateTexture2d {
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
    },
    DestroyResource {
        resource_handle: u32,
    },
    ResourceDirtyRange {
        resource_handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
    },
    UploadResource {
        resource_handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
        data: &'a [u8],
    },

    // Shaders
    CreateShaderDxbc {
        shader_handle: u32,
        stage: u32,
        dxbc_size_bytes: u32,
        dxbc_bytes: &'a [u8],
    },
    DestroyShader {
        shader_handle: u32,
    },
    BindShaders {
        vs: u32,
        ps: u32,
        cs: u32,
    },
    SetShaderConstantsF {
        stage: u32,
        start_register: u32,
        vec4_count: u32,
        data: &'a [u8],
    },

    // Input layouts
    CreateInputLayout {
        input_layout_handle: u32,
        blob_size_bytes: u32,
        blob_bytes: &'a [u8],
    },
    DestroyInputLayout {
        input_layout_handle: u32,
    },
    SetInputLayout {
        input_layout_handle: u32,
    },

    // Pipeline state
    SetBlendState {
        state: AeroGpuBlendState,
    },
    SetDepthStencilState {
        state: AeroGpuDepthStencilState,
    },
    SetRasterizerState {
        state: AeroGpuRasterizerState,
    },

    // Render targets + dynamic state
    SetRenderTargets {
        color_count: u32,
        depth_stencil: u32,
        colors: [u32; AEROGPU_MAX_RENDER_TARGETS],
    },
    SetViewport {
        x_f32: u32,
        y_f32: u32,
        width_f32: u32,
        height_f32: u32,
        min_depth_f32: u32,
        max_depth_f32: u32,
    },
    SetScissor {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },

    // Input assembler
    SetVertexBuffers {
        start_slot: u32,
        buffer_count: u32,
        bindings_bytes: &'a [u8],
    },
    SetIndexBuffer {
        buffer: u32,
        format: u32,
        offset_bytes: u32,
    },
    SetPrimitiveTopology {
        topology: u32,
    },

    // Resource binding / state
    SetTexture {
        shader_stage: u32,
        slot: u32,
        texture: u32,
    },
    SetSamplerState {
        shader_stage: u32,
        slot: u32,
        state: u32,
        value: u32,
    },
    SetRenderState {
        state: u32,
        value: u32,
    },

    // Drawing
    Clear {
        flags: u32,
        color_rgba_f32: [u32; 4],
        depth_f32: u32,
        stencil: u32,
    },
    Draw {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    DrawIndexed {
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    },

    /// Unrecognized opcode; payload is the bytes after `AeroGpuCmdHdr`.
    Unknown {
        opcode: u32,
        payload: &'a [u8],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AeroGpuCmdStreamParseError {
    BufferTooSmall,
    InvalidMagic(u32),
    InvalidSizeBytes { size_bytes: u32, buffer_len: usize },
    InvalidCmdSizeBytes(u32),
    MisalignedCmdSizeBytes(u32),
}

impl fmt::Display for AeroGpuCmdStreamParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AeroGpuCmdStreamParseError::BufferTooSmall => write!(f, "buffer too small"),
            AeroGpuCmdStreamParseError::InvalidMagic(magic) => {
                write!(f, "invalid command stream magic 0x{magic:08X}")
            }
            AeroGpuCmdStreamParseError::InvalidSizeBytes {
                size_bytes,
                buffer_len,
            } => write!(
                f,
                "invalid command stream size_bytes={size_bytes} (buffer_len={buffer_len})"
            ),
            AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(size) => {
                write!(f, "invalid command packet size_bytes={size}")
            }
            AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(size) => {
                write!(f, "command packet size_bytes={size} is not 4-byte aligned")
            }
        }
    }
}

impl std::error::Error for AeroGpuCmdStreamParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AeroGpuCmdStreamView<'a> {
    pub header: AeroGpuCmdStreamHeader,
    pub cmds: Vec<AeroGpuCmd<'a>>,
}

fn read_i32_le(bytes: &[u8]) -> i32 {
    i32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

fn checked_subslice<'a>(
    bytes: &'a [u8],
    offset: usize,
    len: usize,
) -> Result<&'a [u8], AeroGpuCmdStreamParseError> {
    let end = offset
        .checked_add(len)
        .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
    bytes
        .get(offset..end)
        .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)
}

fn get_u8(bytes: &[u8], offset: usize) -> Result<u8, AeroGpuCmdStreamParseError> {
    bytes
        .get(offset)
        .copied()
        .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)
}

fn get_u32(bytes: &[u8], offset: usize) -> Result<u32, AeroGpuCmdStreamParseError> {
    Ok(read_u32_le(checked_subslice(bytes, offset, 4)?))
}

fn get_i32(bytes: &[u8], offset: usize) -> Result<i32, AeroGpuCmdStreamParseError> {
    Ok(read_i32_le(checked_subslice(bytes, offset, 4)?))
}

fn get_u64(bytes: &[u8], offset: usize) -> Result<u64, AeroGpuCmdStreamParseError> {
    Ok(read_u64_le(checked_subslice(bytes, offset, 8)?))
}

pub fn parse_cmd_stream(
    bytes: &[u8],
) -> Result<AeroGpuCmdStreamView<'_>, AeroGpuCmdStreamParseError> {
    if bytes.len() < STREAM_HEADER_SIZE {
        return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
    }

    let magic = read_u32_le(&bytes[0..4]);
    if magic != AEROGPU_CMD_STREAM_MAGIC {
        return Err(AeroGpuCmdStreamParseError::InvalidMagic(magic));
    }
    let abi_version = read_u32_le(&bytes[4..8]);
    let size_bytes = read_u32_le(&bytes[8..12]);
    let flags = read_u32_le(&bytes[12..16]);
    let reserved0 = read_u32_le(&bytes[16..20]);
    let reserved1 = read_u32_le(&bytes[20..24]);

    let header = AeroGpuCmdStreamHeader {
        magic,
        abi_version,
        size_bytes,
        flags,
        reserved0,
        reserved1,
    };

    let size_bytes_usize = size_bytes as usize;
    if size_bytes_usize < STREAM_HEADER_SIZE || size_bytes_usize > bytes.len() {
        return Err(AeroGpuCmdStreamParseError::InvalidSizeBytes {
            size_bytes,
            buffer_len: bytes.len(),
        });
    }

    let mut cmds = Vec::new();
    let mut offset = STREAM_HEADER_SIZE;
    while offset < size_bytes_usize {
        if offset + CMD_HDR_SIZE > size_bytes_usize {
            return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
        }
        let opcode = read_u32_le(&bytes[offset..offset + 4]);
        let cmd_size_bytes = read_u32_le(&bytes[offset + 4..offset + 8]);
        if cmd_size_bytes < CMD_HDR_SIZE as u32 {
            return Err(AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(
                cmd_size_bytes,
            ));
        }
        if cmd_size_bytes % 4 != 0 {
            return Err(AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(
                cmd_size_bytes,
            ));
        }
        let cmd_size_usize = cmd_size_bytes as usize;
        let end = offset.checked_add(cmd_size_usize).ok_or(
            AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(cmd_size_bytes),
        )?;
        if end > size_bytes_usize {
            return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
        }

        let payload = &bytes[offset + CMD_HDR_SIZE..end];
        let cmd = match AeroGpuOpcode::from_u32(opcode) {
            Some(AeroGpuOpcode::Nop) => AeroGpuCmd::Nop,
            Some(AeroGpuOpcode::DebugMarker) => AeroGpuCmd::DebugMarker { bytes: payload },
            Some(AeroGpuOpcode::CreateBuffer) => {
                // struct aerogpu_cmd_create_buffer (40 bytes total, 32 after hdr)
                if payload.len() < 32 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let buffer_handle = get_u32(payload, 0)?;
                let usage_flags = get_u32(payload, 4)?;
                let size_bytes = get_u64(payload, 8)?;
                let backing_alloc_id = get_u32(payload, 16)?;
                let backing_offset_bytes = get_u32(payload, 20)?;
                AeroGpuCmd::CreateBuffer {
                    buffer_handle,
                    usage_flags,
                    size_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                }
            }
            Some(AeroGpuOpcode::CreateTexture2d) => {
                // struct aerogpu_cmd_create_texture2d (56 bytes total, 48 after hdr)
                if payload.len() < 48 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let texture_handle = get_u32(payload, 0)?;
                let usage_flags = get_u32(payload, 4)?;
                let format = get_u32(payload, 8)?;
                let width = get_u32(payload, 12)?;
                let height = get_u32(payload, 16)?;
                let mip_levels = get_u32(payload, 20)?;
                let array_layers = get_u32(payload, 24)?;
                let row_pitch_bytes = get_u32(payload, 28)?;
                let backing_alloc_id = get_u32(payload, 32)?;
                let backing_offset_bytes = get_u32(payload, 36)?;
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
                }
            }
            Some(AeroGpuOpcode::DestroyResource) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let resource_handle = get_u32(payload, 0)?;
                AeroGpuCmd::DestroyResource { resource_handle }
            }
            Some(AeroGpuOpcode::ResourceDirtyRange) => {
                if payload.len() < 24 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let resource_handle = get_u32(payload, 0)?;
                let offset_bytes = get_u64(payload, 8)?;
                let size_bytes = get_u64(payload, 16)?;
                AeroGpuCmd::ResourceDirtyRange {
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                }
            }
            Some(AeroGpuOpcode::UploadResource) => {
                if payload.len() < 24 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let resource_handle = get_u32(payload, 0)?;
                let offset_bytes = get_u64(payload, 8)?;
                let size_bytes = get_u64(payload, 16)?;
                let data_len = usize::try_from(size_bytes)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data_end = 24usize
                    .checked_add(data_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                if payload.len() < data_end {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let data = &payload[24..data_end];
                AeroGpuCmd::UploadResource {
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                    data,
                }
            }
            Some(AeroGpuOpcode::CreateShaderDxbc) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let shader_handle = get_u32(payload, 0)?;
                let stage = get_u32(payload, 4)?;
                let dxbc_size_bytes = get_u32(payload, 8)?;
                let dxbc_len = dxbc_size_bytes as usize;
                let dxbc_end = 16usize
                    .checked_add(dxbc_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                if payload.len() < dxbc_end {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let dxbc_bytes = &payload[16..dxbc_end];
                AeroGpuCmd::CreateShaderDxbc {
                    shader_handle,
                    stage,
                    dxbc_size_bytes,
                    dxbc_bytes,
                }
            }
            Some(AeroGpuOpcode::DestroyShader) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let shader_handle = get_u32(payload, 0)?;
                AeroGpuCmd::DestroyShader { shader_handle }
            }
            Some(AeroGpuOpcode::BindShaders) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let vs = get_u32(payload, 0)?;
                let ps = get_u32(payload, 4)?;
                let cs = get_u32(payload, 8)?;
                AeroGpuCmd::BindShaders { vs, ps, cs }
            }
            Some(AeroGpuOpcode::SetShaderConstantsF) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let stage = get_u32(payload, 0)?;
                let start_register = get_u32(payload, 4)?;
                let vec4_count = get_u32(payload, 8)?;
                let data_len = (vec4_count as usize)
                    .checked_mul(16)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data_end = 16usize
                    .checked_add(data_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                if payload.len() < data_end {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let data = &payload[16..data_end];
                AeroGpuCmd::SetShaderConstantsF {
                    stage,
                    start_register,
                    vec4_count,
                    data,
                }
            }
            Some(AeroGpuOpcode::CreateInputLayout) => {
                if payload.len() < 12 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let input_layout_handle = get_u32(payload, 0)?;
                let blob_size_bytes = get_u32(payload, 4)?;
                let blob_len = blob_size_bytes as usize;
                let blob_end = 12usize
                    .checked_add(blob_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                if payload.len() < blob_end {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let blob_bytes = &payload[12..blob_end];
                AeroGpuCmd::CreateInputLayout {
                    input_layout_handle,
                    blob_size_bytes,
                    blob_bytes,
                }
            }
            Some(AeroGpuOpcode::DestroyInputLayout) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let input_layout_handle = get_u32(payload, 0)?;
                AeroGpuCmd::DestroyInputLayout {
                    input_layout_handle,
                }
            }
            Some(AeroGpuOpcode::SetInputLayout) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let input_layout_handle = get_u32(payload, 0)?;
                AeroGpuCmd::SetInputLayout {
                    input_layout_handle,
                }
            }
            Some(AeroGpuOpcode::SetBlendState) => {
                if payload.len() < 20 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let enable = get_u32(payload, 0)?;
                let src_factor = get_u32(payload, 4)?;
                let dst_factor = get_u32(payload, 8)?;
                let blend_op = get_u32(payload, 12)?;
                let color_write_mask = get_u8(payload, 16)?;
                AeroGpuCmd::SetBlendState {
                    state: AeroGpuBlendState {
                        enable,
                        src_factor,
                        dst_factor,
                        blend_op,
                        color_write_mask,
                    },
                }
            }
            Some(AeroGpuOpcode::SetDepthStencilState) => {
                if payload.len() < 20 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let depth_enable = get_u32(payload, 0)?;
                let depth_write_enable = get_u32(payload, 4)?;
                let depth_func = get_u32(payload, 8)?;
                let stencil_enable = get_u32(payload, 12)?;
                let stencil_read_mask = get_u8(payload, 16)?;
                let stencil_write_mask = get_u8(payload, 17)?;
                AeroGpuCmd::SetDepthStencilState {
                    state: AeroGpuDepthStencilState {
                        depth_enable,
                        depth_write_enable,
                        depth_func,
                        stencil_enable,
                        stencil_read_mask,
                        stencil_write_mask,
                    },
                }
            }
            Some(AeroGpuOpcode::SetRasterizerState) => {
                if payload.len() < 24 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let fill_mode = get_u32(payload, 0)?;
                let cull_mode = get_u32(payload, 4)?;
                let front_ccw = get_u32(payload, 8)?;
                let scissor_enable = get_u32(payload, 12)?;
                let depth_bias = get_i32(payload, 16)?;
                AeroGpuCmd::SetRasterizerState {
                    state: AeroGpuRasterizerState {
                        fill_mode,
                        cull_mode,
                        front_ccw,
                        scissor_enable,
                        depth_bias,
                    },
                }
            }
            Some(AeroGpuOpcode::SetRenderTargets) => {
                if payload.len() < 40 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let color_count = get_u32(payload, 0)?;
                let depth_stencil = get_u32(payload, 4)?;
                let mut colors = [0u32; AEROGPU_MAX_RENDER_TARGETS];
                for (idx, slot) in colors.iter_mut().enumerate() {
                    *slot = get_u32(payload, 8 + idx * 4)?;
                }
                AeroGpuCmd::SetRenderTargets {
                    color_count,
                    depth_stencil,
                    colors,
                }
            }
            Some(AeroGpuOpcode::SetViewport) => {
                if payload.len() < 24 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let x_f32 = get_u32(payload, 0)?;
                let y_f32 = get_u32(payload, 4)?;
                let width_f32 = get_u32(payload, 8)?;
                let height_f32 = get_u32(payload, 12)?;
                let min_depth_f32 = get_u32(payload, 16)?;
                let max_depth_f32 = get_u32(payload, 20)?;
                AeroGpuCmd::SetViewport {
                    x_f32,
                    y_f32,
                    width_f32,
                    height_f32,
                    min_depth_f32,
                    max_depth_f32,
                }
            }
            Some(AeroGpuOpcode::SetScissor) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let x = get_i32(payload, 0)?;
                let y = get_i32(payload, 4)?;
                let width = get_i32(payload, 8)?;
                let height = get_i32(payload, 12)?;
                AeroGpuCmd::SetScissor {
                    x,
                    y,
                    width,
                    height,
                }
            }
            Some(AeroGpuOpcode::SetVertexBuffers) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let start_slot = get_u32(payload, 0)?;
                let buffer_count = get_u32(payload, 4)?;
                let bindings_len = (buffer_count as usize)
                    .checked_mul(VERTEX_BUFFER_BINDING_SIZE)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_end = 8usize
                    .checked_add(bindings_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                if payload.len() < bindings_end {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let bindings_bytes = &payload[8..bindings_end];
                AeroGpuCmd::SetVertexBuffers {
                    start_slot,
                    buffer_count,
                    bindings_bytes,
                }
            }
            Some(AeroGpuOpcode::SetIndexBuffer) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let buffer = get_u32(payload, 0)?;
                let format = get_u32(payload, 4)?;
                let offset_bytes = get_u32(payload, 8)?;
                AeroGpuCmd::SetIndexBuffer {
                    buffer,
                    format,
                    offset_bytes,
                }
            }
            Some(AeroGpuOpcode::SetPrimitiveTopology) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let topology = get_u32(payload, 0)?;
                AeroGpuCmd::SetPrimitiveTopology { topology }
            }
            Some(AeroGpuOpcode::SetTexture) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let shader_stage = get_u32(payload, 0)?;
                let slot = get_u32(payload, 4)?;
                let texture = get_u32(payload, 8)?;
                AeroGpuCmd::SetTexture {
                    shader_stage,
                    slot,
                    texture,
                }
            }
            Some(AeroGpuOpcode::SetSamplerState) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let shader_stage = get_u32(payload, 0)?;
                let slot = get_u32(payload, 4)?;
                let state = get_u32(payload, 8)?;
                let value = get_u32(payload, 12)?;
                AeroGpuCmd::SetSamplerState {
                    shader_stage,
                    slot,
                    state,
                    value,
                }
            }
            Some(AeroGpuOpcode::SetRenderState) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let state = get_u32(payload, 0)?;
                let value = get_u32(payload, 4)?;
                AeroGpuCmd::SetRenderState { state, value }
            }
            Some(AeroGpuOpcode::Clear) => {
                if payload.len() < 28 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let flags = get_u32(payload, 0)?;
                let mut color_rgba_f32 = [0u32; 4];
                for (idx, slot) in color_rgba_f32.iter_mut().enumerate() {
                    *slot = get_u32(payload, 4 + idx * 4)?;
                }
                let depth_f32 = get_u32(payload, 20)?;
                let stencil = get_u32(payload, 24)?;
                AeroGpuCmd::Clear {
                    flags,
                    color_rgba_f32,
                    depth_f32,
                    stencil,
                }
            }
            Some(AeroGpuOpcode::Draw) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let vertex_count = get_u32(payload, 0)?;
                let instance_count = get_u32(payload, 4)?;
                let first_vertex = get_u32(payload, 8)?;
                let first_instance = get_u32(payload, 12)?;
                AeroGpuCmd::Draw {
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                }
            }
            Some(AeroGpuOpcode::DrawIndexed) => {
                if payload.len() < 20 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let index_count = get_u32(payload, 0)?;
                let instance_count = get_u32(payload, 4)?;
                let first_index = get_u32(payload, 8)?;
                let base_vertex = get_i32(payload, 12)?;
                let first_instance = get_u32(payload, 16)?;
                AeroGpuCmd::DrawIndexed {
                    index_count,
                    instance_count,
                    first_index,
                    base_vertex,
                    first_instance,
                }
            }
            Some(AeroGpuOpcode::Present) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let scanout_id = read_u32_le(&payload[0..4]);
                let flags = read_u32_le(&payload[4..8]);
                AeroGpuCmd::Present { scanout_id, flags }
            }
            Some(AeroGpuOpcode::PresentEx) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let scanout_id = read_u32_le(&payload[0..4]);
                let flags = read_u32_le(&payload[4..8]);
                let d3d9_present_flags = read_u32_le(&payload[8..12]);
                AeroGpuCmd::PresentEx {
                    scanout_id,
                    flags,
                    d3d9_present_flags,
                }
            }
            Some(AeroGpuOpcode::ExportSharedSurface) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let resource_handle = get_u32(payload, 0)?;
                let share_token = get_u64(payload, 8)?;
                AeroGpuCmd::ExportSharedSurface {
                    resource_handle,
                    share_token,
                }
            }
            Some(AeroGpuOpcode::ImportSharedSurface) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let out_resource_handle = get_u32(payload, 0)?;
                let share_token = get_u64(payload, 8)?;
                AeroGpuCmd::ImportSharedSurface {
                    out_resource_handle,
                    share_token,
                }
            }
            Some(AeroGpuOpcode::Flush) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                AeroGpuCmd::Flush
            }
            None => AeroGpuCmd::Unknown { opcode, payload },
        };

        cmds.push(cmd);
        offset = end;
    }

    Ok(AeroGpuCmdStreamView { header, cmds })
}
