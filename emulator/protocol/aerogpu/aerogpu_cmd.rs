//! AeroGPU command stream layouts.
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_cmd.h`.

use super::aerogpu_pci::{parse_and_validate_abi_version_u32, AerogpuAbiError};

pub type AerogpuHandle = u32;

pub const AEROGPU_CMD_STREAM_MAGIC: u32 = 0x444D_4341; // "ACMD" LE

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCmdStreamFlags {
    None = 0,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdStreamHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub flags: u32,
    pub reserved0: u32,
    pub reserved1: u32,
}

impl AerogpuCmdStreamHeader {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdHdr {
    pub opcode: u32,
    pub size_bytes: u32,
}

impl AerogpuCmdHdr {
    pub const SIZE_BYTES: usize = 8;
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCmdOpcode {
    Nop = 0,
    DebugMarker = 1,

    CreateBuffer = 0x100,
    CreateTexture2d = 0x101,
    DestroyResource = 0x102,
    ResourceDirtyRange = 0x103,

    CreateShaderDxbc = 0x200,
    DestroyShader = 0x201,
    BindShaders = 0x202,

    SetBlendState = 0x300,
    SetDepthStencilState = 0x301,
    SetRasterizerState = 0x302,

    SetRenderTargets = 0x400,
    SetViewport = 0x401,
    SetScissor = 0x402,

    SetVertexBuffers = 0x500,
    SetIndexBuffer = 0x501,

    Clear = 0x600,
    Draw = 0x601,
    DrawIndexed = 0x602,

    Present = 0x700,
}

impl AerogpuCmdOpcode {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Nop),
            1 => Some(Self::DebugMarker),
            0x100 => Some(Self::CreateBuffer),
            0x101 => Some(Self::CreateTexture2d),
            0x102 => Some(Self::DestroyResource),
            0x103 => Some(Self::ResourceDirtyRange),
            0x200 => Some(Self::CreateShaderDxbc),
            0x201 => Some(Self::DestroyShader),
            0x202 => Some(Self::BindShaders),
            0x300 => Some(Self::SetBlendState),
            0x301 => Some(Self::SetDepthStencilState),
            0x302 => Some(Self::SetRasterizerState),
            0x400 => Some(Self::SetRenderTargets),
            0x401 => Some(Self::SetViewport),
            0x402 => Some(Self::SetScissor),
            0x500 => Some(Self::SetVertexBuffers),
            0x501 => Some(Self::SetIndexBuffer),
            0x600 => Some(Self::Clear),
            0x601 => Some(Self::Draw),
            0x602 => Some(Self::DrawIndexed),
            0x700 => Some(Self::Present),
            _ => None,
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuShaderStage {
    Vertex = 0,
    Pixel = 1,
    Compute = 2,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuIndexFormat {
    Uint16 = 0,
    Uint32 = 1,
}

pub const AEROGPU_RESOURCE_USAGE_NONE: u32 = 0;
pub const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = 1u32 << 0;
pub const AEROGPU_RESOURCE_USAGE_INDEX_BUFFER: u32 = 1u32 << 1;
pub const AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER: u32 = 1u32 << 2;
pub const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = 1u32 << 3;
pub const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1u32 << 4;
pub const AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL: u32 = 1u32 << 5;
pub const AEROGPU_RESOURCE_USAGE_SCANOUT: u32 = 1u32 << 6;

/* --------------------------- Resource management -------------------------- */

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCreateBuffer {
    pub hdr: AerogpuCmdHdr,
    pub buffer_handle: AerogpuHandle,
    pub usage_flags: u32,
    pub size_bytes: u64,
    pub backing_alloc_id: u32,
    pub backing_offset_bytes: u32,
    pub reserved0: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCreateTexture2d {
    pub hdr: AerogpuCmdHdr,
    pub texture_handle: AerogpuHandle,
    pub usage_flags: u32,
    pub format: u32, // aerogpu_format
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub row_pitch_bytes: u32,
    pub backing_alloc_id: u32,
    pub backing_offset_bytes: u32,
    pub reserved0: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDestroyResource {
    pub hdr: AerogpuCmdHdr,
    pub resource_handle: AerogpuHandle,
    pub reserved0: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdResourceDirtyRange {
    pub hdr: AerogpuCmdHdr,
    pub resource_handle: AerogpuHandle,
    pub reserved0: u32,
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

/* -------------------------------- Shaders -------------------------------- */

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCreateShaderDxbc {
    pub hdr: AerogpuCmdHdr,
    pub shader_handle: AerogpuHandle,
    pub stage: u32,
    pub dxbc_size_bytes: u32,
    pub reserved0: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDestroyShader {
    pub hdr: AerogpuCmdHdr,
    pub shader_handle: AerogpuHandle,
    pub reserved0: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdBindShaders {
    pub hdr: AerogpuCmdHdr,
    pub vs: AerogpuHandle,
    pub ps: AerogpuHandle,
    pub cs: AerogpuHandle,
    pub reserved0: u32,
}

/* ------------------------------ Pipeline state ---------------------------- */

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuBlendFactor {
    Zero = 0,
    One = 1,
    SrcAlpha = 2,
    InvSrcAlpha = 3,
    DestAlpha = 4,
    InvDestAlpha = 5,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuBlendOp {
    Add = 0,
    Subtract = 1,
    RevSubtract = 2,
    Min = 3,
    Max = 4,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuBlendState {
    pub enable: u32,
    pub src_factor: u32,
    pub dst_factor: u32,
    pub blend_op: u32,
    pub color_write_mask: u8,
    pub reserved0: [u8; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetBlendState {
    pub hdr: AerogpuCmdHdr,
    pub state: AerogpuBlendState,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCompareFunc {
    Never = 0,
    Less = 1,
    Equal = 2,
    LessEqual = 3,
    Greater = 4,
    NotEqual = 5,
    GreaterEqual = 6,
    Always = 7,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuDepthStencilState {
    pub depth_enable: u32,
    pub depth_write_enable: u32,
    pub depth_func: u32,
    pub stencil_enable: u32,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,
    pub reserved0: [u8; 2],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetDepthStencilState {
    pub hdr: AerogpuCmdHdr,
    pub state: AerogpuDepthStencilState,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuFillMode {
    Solid = 0,
    Wireframe = 1,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCullMode {
    None = 0,
    Front = 1,
    Back = 2,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuRasterizerState {
    pub fill_mode: u32,
    pub cull_mode: u32,
    pub front_ccw: u32,
    pub scissor_enable: u32,
    pub depth_bias: i32,
    pub reserved0: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetRasterizerState {
    pub hdr: AerogpuCmdHdr,
    pub state: AerogpuRasterizerState,
}

/* ------------------------- Render targets / state ------------------------- */

pub const AEROGPU_MAX_RENDER_TARGETS: usize = 8;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetRenderTargets {
    pub hdr: AerogpuCmdHdr,
    pub color_count: u32,
    pub depth_stencil: AerogpuHandle,
    pub colors: [AerogpuHandle; AEROGPU_MAX_RENDER_TARGETS],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetViewport {
    pub hdr: AerogpuCmdHdr,
    pub x_f32: u32,
    pub y_f32: u32,
    pub width_f32: u32,
    pub height_f32: u32,
    pub min_depth_f32: u32,
    pub max_depth_f32: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetScissor {
    pub hdr: AerogpuCmdHdr,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/* ------------------------------ Input assembler --------------------------- */

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuVertexBufferBinding {
    pub buffer: AerogpuHandle,
    pub stride_bytes: u32,
    pub offset_bytes: u32,
    pub reserved0: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetVertexBuffers {
    pub hdr: AerogpuCmdHdr,
    pub start_slot: u32,
    pub buffer_count: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetIndexBuffer {
    pub hdr: AerogpuCmdHdr,
    pub buffer: AerogpuHandle,
    pub format: u32,
    pub offset_bytes: u32,
    pub reserved0: u32,
}

/* -------------------------------- Drawing -------------------------------- */

pub const AEROGPU_CLEAR_COLOR: u32 = 1u32 << 0;
pub const AEROGPU_CLEAR_DEPTH: u32 = 1u32 << 1;
pub const AEROGPU_CLEAR_STENCIL: u32 = 1u32 << 2;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdClear {
    pub hdr: AerogpuCmdHdr,
    pub flags: u32,
    pub color_rgba_f32: [u32; 4],
    pub depth_f32: u32,
    pub stencil: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDraw {
    pub hdr: AerogpuCmdHdr,
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDrawIndexed {
    pub hdr: AerogpuCmdHdr,
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
}

/* ------------------------------ Presentation ------------------------------ */

pub const AEROGPU_PRESENT_FLAG_NONE: u32 = 0;
pub const AEROGPU_PRESENT_FLAG_VSYNC: u32 = 1u32 << 0;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdPresent {
    pub hdr: AerogpuCmdHdr,
    pub scanout_id: u32,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCmdDecodeError {
    BufferTooSmall,
    BadMagic { found: u32 },
    Abi(AerogpuAbiError),
    BadSizeBytes { found: u32 },
    SizeNotAligned { found: u32 },
}

impl From<AerogpuAbiError> for AerogpuCmdDecodeError {
    fn from(value: AerogpuAbiError) -> Self {
        Self::Abi(value)
    }
}

pub fn decode_cmd_stream_header_le(buf: &[u8]) -> Result<AerogpuCmdStreamHeader, AerogpuCmdDecodeError> {
    if buf.len() < AerogpuCmdStreamHeader::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let abi_version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    let size_bytes = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let flags = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let reserved0 = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    let reserved1 = u32::from_le_bytes(buf[20..24].try_into().unwrap());

    let hdr = AerogpuCmdStreamHeader {
        magic,
        abi_version,
        size_bytes,
        flags,
        reserved0,
        reserved1,
    };

    validate_cmd_stream_header(&hdr)?;
    Ok(hdr)
}

pub fn validate_cmd_stream_header(hdr: &AerogpuCmdStreamHeader) -> Result<(), AerogpuCmdDecodeError> {
    if hdr.magic != AEROGPU_CMD_STREAM_MAGIC {
        return Err(AerogpuCmdDecodeError::BadMagic { found: hdr.magic });
    }

    let _ = parse_and_validate_abi_version_u32(hdr.abi_version)?;

    if hdr.size_bytes < AerogpuCmdStreamHeader::SIZE_BYTES as u32 {
        return Err(AerogpuCmdDecodeError::BadSizeBytes {
            found: hdr.size_bytes,
        });
    }

    Ok(())
}

pub fn decode_cmd_hdr_le(buf: &[u8]) -> Result<AerogpuCmdHdr, AerogpuCmdDecodeError> {
    if buf.len() < AerogpuCmdHdr::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let opcode = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let size_bytes = u32::from_le_bytes(buf[4..8].try_into().unwrap());

    if size_bytes < AerogpuCmdHdr::SIZE_BYTES as u32 {
        return Err(AerogpuCmdDecodeError::BadSizeBytes { found: size_bytes });
    }
    if size_bytes % 4 != 0 {
        return Err(AerogpuCmdDecodeError::SizeNotAligned { found: size_bytes });
    }

    Ok(AerogpuCmdHdr { opcode, size_bytes })
}

