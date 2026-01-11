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
    UploadResource = 0x104,

    CreateShaderDxbc = 0x200,
    DestroyShader = 0x201,
    BindShaders = 0x202,
    SetShaderConstantsF = 0x203,
    CreateInputLayout = 0x204,
    DestroyInputLayout = 0x205,
    SetInputLayout = 0x206,

    SetBlendState = 0x300,
    SetDepthStencilState = 0x301,
    SetRasterizerState = 0x302,

    SetRenderTargets = 0x400,
    SetViewport = 0x401,
    SetScissor = 0x402,

    SetVertexBuffers = 0x500,
    SetIndexBuffer = 0x501,
    SetPrimitiveTopology = 0x502,
    SetTexture = 0x510,
    SetSamplerState = 0x511,
    SetRenderState = 0x512,

    Clear = 0x600,
    Draw = 0x601,
    DrawIndexed = 0x602,

    Present = 0x700,
    PresentEx = 0x701,

    ExportSharedSurface = 0x710,
    ImportSharedSurface = 0x711,

    Flush = 0x720,
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
            0x104 => Some(Self::UploadResource),
            0x200 => Some(Self::CreateShaderDxbc),
            0x201 => Some(Self::DestroyShader),
            0x202 => Some(Self::BindShaders),
            0x203 => Some(Self::SetShaderConstantsF),
            0x204 => Some(Self::CreateInputLayout),
            0x205 => Some(Self::DestroyInputLayout),
            0x206 => Some(Self::SetInputLayout),
            0x300 => Some(Self::SetBlendState),
            0x301 => Some(Self::SetDepthStencilState),
            0x302 => Some(Self::SetRasterizerState),
            0x400 => Some(Self::SetRenderTargets),
            0x401 => Some(Self::SetViewport),
            0x402 => Some(Self::SetScissor),
            0x500 => Some(Self::SetVertexBuffers),
            0x501 => Some(Self::SetIndexBuffer),
            0x502 => Some(Self::SetPrimitiveTopology),
            0x510 => Some(Self::SetTexture),
            0x511 => Some(Self::SetSamplerState),
            0x512 => Some(Self::SetRenderState),
            0x600 => Some(Self::Clear),
            0x601 => Some(Self::Draw),
            0x602 => Some(Self::DrawIndexed),
            0x700 => Some(Self::Present),
            0x701 => Some(Self::PresentEx),
            0x710 => Some(Self::ExportSharedSurface),
            0x711 => Some(Self::ImportSharedSurface),
            0x720 => Some(Self::Flush),
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

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuPrimitiveTopology {
    PointList = 1,
    LineList = 2,
    LineStrip = 3,
    TriangleList = 4,
    TriangleStrip = 5,
    TriangleFan = 6,
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

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdUploadResource {
    pub hdr: AerogpuCmdHdr,
    pub resource_handle: AerogpuHandle,
    pub reserved0: u32,
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

impl AerogpuCmdUploadResource {
    pub const SIZE_BYTES: usize = 32;
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

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetShaderConstantsF {
    pub hdr: AerogpuCmdHdr,
    pub stage: u32,
    pub start_register: u32,
    pub vec4_count: u32,
    pub reserved0: u32,
}

impl AerogpuCmdSetShaderConstantsF {
    pub const SIZE_BYTES: usize = 24;
}

pub const AEROGPU_INPUT_LAYOUT_BLOB_MAGIC: u32 = 0x5941_4C49; // "ILAY" LE
pub const AEROGPU_INPUT_LAYOUT_BLOB_VERSION: u32 = 1;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuInputLayoutBlobHeader {
    pub magic: u32,
    pub version: u32,
    pub element_count: u32,
    pub reserved0: u32,
}

impl AerogpuInputLayoutBlobHeader {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuInputLayoutElementDxgi {
    pub semantic_name_hash: u32,
    pub semantic_index: u32,
    pub dxgi_format: u32,
    pub input_slot: u32,
    pub aligned_byte_offset: u32,
    pub input_slot_class: u32,
    pub instance_data_step_rate: u32,
}

impl AerogpuInputLayoutElementDxgi {
    pub const SIZE_BYTES: usize = 28;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCreateInputLayout {
    pub hdr: AerogpuCmdHdr,
    pub input_layout_handle: AerogpuHandle,
    pub blob_size_bytes: u32,
    pub reserved0: u32,
}

impl AerogpuCmdCreateInputLayout {
    pub const SIZE_BYTES: usize = 20;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDestroyInputLayout {
    pub hdr: AerogpuCmdHdr,
    pub input_layout_handle: AerogpuHandle,
    pub reserved0: u32,
}

impl AerogpuCmdDestroyInputLayout {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetInputLayout {
    pub hdr: AerogpuCmdHdr,
    pub input_layout_handle: AerogpuHandle,
    pub reserved0: u32,
}

impl AerogpuCmdSetInputLayout {
    pub const SIZE_BYTES: usize = 16;
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

impl AerogpuVertexBufferBinding {
    pub const SIZE_BYTES: usize = 16;
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

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetPrimitiveTopology {
    pub hdr: AerogpuCmdHdr,
    pub topology: u32,
    pub reserved0: u32,
}

impl AerogpuCmdSetPrimitiveTopology {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetTexture {
    pub hdr: AerogpuCmdHdr,
    pub shader_stage: u32,
    pub slot: u32,
    pub texture: AerogpuHandle,
    pub reserved0: u32,
}

impl AerogpuCmdSetTexture {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetSamplerState {
    pub hdr: AerogpuCmdHdr,
    pub shader_stage: u32,
    pub slot: u32,
    pub state: u32,
    pub value: u32,
}

impl AerogpuCmdSetSamplerState {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetRenderState {
    pub hdr: AerogpuCmdHdr,
    pub state: u32,
    pub value: u32,
}

impl AerogpuCmdSetRenderState {
    pub const SIZE_BYTES: usize = 16;
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

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdPresentEx {
    pub hdr: AerogpuCmdHdr,
    pub scanout_id: u32,
    pub flags: u32,
    pub d3d9_present_flags: u32,
    pub reserved0: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdExportSharedSurface {
    pub hdr: AerogpuCmdHdr,
    pub resource_handle: AerogpuHandle,
    pub reserved0: u32,
    pub share_token: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdImportSharedSurface {
    pub hdr: AerogpuCmdHdr,
    pub out_resource_handle: AerogpuHandle,
    pub reserved0: u32,
    pub share_token: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdFlush {
    pub hdr: AerogpuCmdHdr,
    pub reserved0: u32,
    pub reserved1: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCmdDecodeError {
    BufferTooSmall,
    BadMagic { found: u32 },
    Abi(AerogpuAbiError),
    BadSizeBytes { found: u32 },
    SizeNotAligned { found: u32 },
    PacketOverrunsStream {
        offset: u32,
        packet_size_bytes: u32,
        stream_size_bytes: u32,
    },
    UnexpectedOpcode {
        found: u32,
        expected: AerogpuCmdOpcode,
    },
    PayloadSizeMismatch {
        expected: usize,
        found: usize,
    },
    CountOverflow,
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

fn validate_packet_len(buf: &[u8], hdr: AerogpuCmdHdr) -> Result<usize, AerogpuCmdDecodeError> {
    let packet_len = hdr.size_bytes as usize;
    if buf.len() < packet_len {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }
    Ok(packet_len)
}

/// Decode CREATE_SHADER_DXBC and return the DXBC byte payload (without padding).
pub fn decode_cmd_create_shader_dxbc_payload_le(
    buf: &[u8],
) -> Result<(AerogpuCmdCreateShaderDxbc, &[u8]), AerogpuCmdDecodeError> {
    if buf.len() < 24 {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;

    let dxbc_size_bytes = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    let payload_start = 24usize;
    let payload_end = payload_start
        .checked_add(dxbc_size_bytes as usize)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    if payload_end > packet_len {
        return Err(AerogpuCmdDecodeError::BadSizeBytes { found: hdr.size_bytes });
    }

    let cmd = AerogpuCmdCreateShaderDxbc {
        hdr,
        shader_handle: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        stage: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        dxbc_size_bytes,
        reserved0: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
    };

    Ok((cmd, &buf[payload_start..payload_end]))
}

/// Decode CREATE_INPUT_LAYOUT and return the blob payload (without padding).
pub fn decode_cmd_create_input_layout_blob_le(
    buf: &[u8],
) -> Result<(AerogpuCmdCreateInputLayout, &[u8]), AerogpuCmdDecodeError> {
    if buf.len() < AerogpuCmdCreateInputLayout::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;

    let blob_size_bytes = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let payload_start = AerogpuCmdCreateInputLayout::SIZE_BYTES;
    let payload_end = payload_start
        .checked_add(blob_size_bytes as usize)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    if payload_end > packet_len {
        return Err(AerogpuCmdDecodeError::BadSizeBytes { found: hdr.size_bytes });
    }

    let cmd = AerogpuCmdCreateInputLayout {
        hdr,
        input_layout_handle: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        blob_size_bytes,
        reserved0: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
    };

    Ok((cmd, &buf[payload_start..payload_end]))
}

/// Decode SET_SHADER_CONSTANTS_F and return the float payload.
pub fn decode_cmd_set_shader_constants_f_payload_le(
    buf: &[u8],
) -> Result<(AerogpuCmdSetShaderConstantsF, Vec<f32>), AerogpuCmdDecodeError> {
    if buf.len() < AerogpuCmdSetShaderConstantsF::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;

    let vec4_count = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    let float_count = vec4_count
        .checked_mul(4)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)? as usize;
    let payload_size_bytes = float_count
        .checked_mul(4)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    let payload_start = AerogpuCmdSetShaderConstantsF::SIZE_BYTES;
    let payload_end = payload_start
        .checked_add(payload_size_bytes)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    if payload_end > packet_len {
        return Err(AerogpuCmdDecodeError::BadSizeBytes { found: hdr.size_bytes });
    }

    let cmd = AerogpuCmdSetShaderConstantsF {
        hdr,
        stage: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        start_register: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        vec4_count,
        reserved0: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
    };

    let mut out = Vec::with_capacity(float_count);
    for i in 0..float_count {
        let off = payload_start + i * 4;
        let bits = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        out.push(f32::from_bits(bits));
    }

    Ok((cmd, out))
}

/// Decode UPLOAD_RESOURCE and return the raw payload bytes (without padding).
pub fn decode_cmd_upload_resource_payload_le(
    buf: &[u8],
) -> Result<(AerogpuCmdUploadResource, &[u8]), AerogpuCmdDecodeError> {
    if buf.len() < AerogpuCmdUploadResource::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;

    let size_bytes_u64 = u64::from_le_bytes(buf[24..32].try_into().unwrap());
    let data_len = usize::try_from(size_bytes_u64).map_err(|_| AerogpuCmdDecodeError::BadSizeBytes {
        found: hdr.size_bytes,
    })?;
    let payload_start = AerogpuCmdUploadResource::SIZE_BYTES;
    let payload_end = payload_start
        .checked_add(data_len)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    if payload_end > packet_len {
        return Err(AerogpuCmdDecodeError::BadSizeBytes { found: hdr.size_bytes });
    }

    let cmd = AerogpuCmdUploadResource {
        hdr,
        resource_handle: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        reserved0: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        offset_bytes: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
        size_bytes: size_bytes_u64,
    };

    Ok((cmd, &buf[payload_start..payload_end]))
}

/// Decode SET_VERTEX_BUFFERS and parse the trailing `aerogpu_vertex_buffer_binding[]`.
pub fn decode_cmd_set_vertex_buffers_bindings_le(
    buf: &[u8],
) -> Result<(AerogpuCmdSetVertexBuffers, Vec<AerogpuVertexBufferBinding>), AerogpuCmdDecodeError> {
    if buf.len() < 16 {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;

    let buffer_count = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let bindings_size_bytes = buffer_count
        .checked_mul(AerogpuVertexBufferBinding::SIZE_BYTES as u32)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)? as usize;
    let payload_start = 16usize;
    let payload_end = payload_start
        .checked_add(bindings_size_bytes)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    if payload_end > packet_len {
        return Err(AerogpuCmdDecodeError::BadSizeBytes { found: hdr.size_bytes });
    }

    let cmd = AerogpuCmdSetVertexBuffers {
        hdr,
        start_slot: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        buffer_count,
    };

    let mut bindings = Vec::with_capacity(buffer_count as usize);
    for i in 0..(buffer_count as usize) {
        let off = payload_start + i * AerogpuVertexBufferBinding::SIZE_BYTES;
        let binding = AerogpuVertexBufferBinding {
            buffer: u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()),
            stride_bytes: u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()),
            offset_bytes: u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap()),
            reserved0: u32::from_le_bytes(buf[off + 12..off + 16].try_into().unwrap()),
        };
        bindings.push(binding);
    }

    Ok((cmd, bindings))
}

#[derive(Clone, Copy)]
pub struct AerogpuCmdPacket<'a> {
    pub hdr: AerogpuCmdHdr,
    pub opcode: Option<AerogpuCmdOpcode>,
    pub payload: &'a [u8],
}

pub struct AerogpuCmdStreamIter<'a> {
    header: AerogpuCmdStreamHeader,
    buf: &'a [u8],
    offset: usize,
    end: usize,
    done: bool,
}

impl<'a> AerogpuCmdStreamIter<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self, AerogpuCmdDecodeError> {
        let header = decode_cmd_stream_header_le(buf)?;
        let end = header.size_bytes as usize;
        if buf.len() < end {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        Ok(Self {
            header,
            buf,
            offset: AerogpuCmdStreamHeader::SIZE_BYTES,
            end,
            done: false,
        })
    }

    pub fn header(&self) -> &AerogpuCmdStreamHeader {
        &self.header
    }
}

impl<'a> Iterator for AerogpuCmdStreamIter<'a> {
    type Item = Result<AerogpuCmdPacket<'a>, AerogpuCmdDecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.offset >= self.end {
            return None;
        }

        let hdr_end = match self.offset.checked_add(AerogpuCmdHdr::SIZE_BYTES) {
            Some(end) => end,
            None => {
                self.done = true;
                return Some(Err(AerogpuCmdDecodeError::CountOverflow));
            }
        };
        if hdr_end > self.end {
            self.done = true;
            return Some(Err(AerogpuCmdDecodeError::BufferTooSmall));
        }

        let hdr = match decode_cmd_hdr_le(&self.buf[self.offset..self.end]) {
            Ok(hdr) => hdr,
            Err(err) => {
                self.done = true;
                return Some(Err(err));
            }
        };

        let packet_size = hdr.size_bytes as usize;
        let packet_end = match self.offset.checked_add(packet_size) {
            Some(end) => end,
            None => {
                self.done = true;
                return Some(Err(AerogpuCmdDecodeError::CountOverflow));
            }
        };
        if packet_end > self.end {
            self.done = true;
            return Some(Err(AerogpuCmdDecodeError::PacketOverrunsStream {
                offset: self.offset as u32,
                packet_size_bytes: hdr.size_bytes,
                stream_size_bytes: self.header.size_bytes,
            }));
        }

        let payload = &self.buf[hdr_end..packet_end];
        let packet = AerogpuCmdPacket {
            hdr,
            opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
            payload,
        };

        self.offset = packet_end;
        Some(Ok(packet))
    }
}

pub struct AerogpuCmdStreamView<'a> {
    pub header: AerogpuCmdStreamHeader,
    pub packets: Vec<AerogpuCmdPacket<'a>>,
}

impl<'a> AerogpuCmdStreamView<'a> {
    pub fn decode_from_le_bytes(buf: &'a [u8]) -> Result<Self, AerogpuCmdDecodeError> {
        let iter = AerogpuCmdStreamIter::new(buf)?;
        let header = *iter.header();
        let packets = iter.collect::<Result<Vec<_>, _>>()?;
        Ok(Self { header, packets })
    }
}

fn align_up_4(size: usize) -> Result<usize, AerogpuCmdDecodeError> {
    size.checked_add(3)
        .map(|v| v & !3usize)
        .ok_or(AerogpuCmdDecodeError::CountOverflow)
}

fn validate_expected_payload_size(expected: usize, payload: &[u8]) -> Result<(), AerogpuCmdDecodeError> {
    if payload.len() != expected {
        return Err(AerogpuCmdDecodeError::PayloadSizeMismatch {
            expected,
            found: payload.len(),
        });
    }
    Ok(())
}

impl<'a> AerogpuCmdPacket<'a> {
    pub fn decode_create_shader_dxbc_payload_le(
        &self,
    ) -> Result<(AerogpuCmdCreateShaderDxbc, &'a [u8]), AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::CreateShaderDxbc) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::CreateShaderDxbc,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let shader_handle = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let stage = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let dxbc_size_bytes = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        let dxbc_size = dxbc_size_bytes as usize;
        let expected_payload_size = 16usize
            .checked_add(align_up_4(dxbc_size)?)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        validate_expected_payload_size(expected_payload_size, self.payload)?;

        let dxbc_bytes = &self.payload[16..16 + dxbc_size];
        Ok((
            AerogpuCmdCreateShaderDxbc {
                hdr: self.hdr,
                shader_handle,
                stage,
                dxbc_size_bytes,
                reserved0,
            },
            dxbc_bytes,
        ))
    }

    pub fn decode_upload_resource_payload_le(
        &self,
    ) -> Result<(AerogpuCmdUploadResource, &'a [u8]), AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::UploadResource) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::UploadResource,
            });
        }
        if self.payload.len() < 24 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let resource_handle = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let offset_bytes = u64::from_le_bytes(self.payload[8..16].try_into().unwrap());
        let size_bytes = u64::from_le_bytes(self.payload[16..24].try_into().unwrap());

        let data_size =
            usize::try_from(size_bytes).map_err(|_| AerogpuCmdDecodeError::BadSizeBytes { found: self.hdr.size_bytes })?;
        let expected_payload_size = 24usize
            .checked_add(align_up_4(data_size)?)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        validate_expected_payload_size(expected_payload_size, self.payload)?;

        let data_bytes = &self.payload[24..24 + data_size];
        Ok((
            AerogpuCmdUploadResource {
                hdr: self.hdr,
                resource_handle,
                reserved0,
                offset_bytes,
                size_bytes,
            },
            data_bytes,
        ))
    }

    pub fn decode_create_input_layout_payload_le(
        &self,
    ) -> Result<(AerogpuCmdCreateInputLayout, &'a [u8]), AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::CreateInputLayout) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::CreateInputLayout,
            });
        }
        if self.payload.len() < 12 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let input_layout_handle = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let blob_size_bytes = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());

        let blob_size = blob_size_bytes as usize;
        let expected_payload_size = 12usize
            .checked_add(align_up_4(blob_size)?)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        validate_expected_payload_size(expected_payload_size, self.payload)?;

        let blob_bytes = &self.payload[12..12 + blob_size];
        Ok((
            AerogpuCmdCreateInputLayout {
                hdr: self.hdr,
                input_layout_handle,
                blob_size_bytes,
                reserved0,
            },
            blob_bytes,
        ))
    }

    pub fn decode_set_vertex_buffers_payload_le(
        &self,
    ) -> Result<(AerogpuCmdSetVertexBuffers, &'a [AerogpuVertexBufferBinding]), AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::SetVertexBuffers) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::SetVertexBuffers,
            });
        }
        if self.payload.len() < 8 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let start_slot = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let buffer_count = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());

        let buffer_count_usize = buffer_count as usize;
        let binding_bytes_len = buffer_count_usize
            .checked_mul(core::mem::size_of::<AerogpuVertexBufferBinding>())
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        let expected_payload_size = 8usize
            .checked_add(binding_bytes_len)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        validate_expected_payload_size(expected_payload_size, self.payload)?;

        let binding_bytes = &self.payload[8..];
        let (prefix, bindings, suffix) = unsafe { binding_bytes.align_to::<AerogpuVertexBufferBinding>() };
        if !prefix.is_empty() || !suffix.is_empty() || bindings.len() != buffer_count_usize {
            return Err(AerogpuCmdDecodeError::CountOverflow);
        }

        Ok((
            AerogpuCmdSetVertexBuffers {
                hdr: self.hdr,
                start_slot,
                buffer_count,
            },
            bindings,
        ))
    }
}
