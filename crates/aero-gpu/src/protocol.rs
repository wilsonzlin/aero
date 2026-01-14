//! AeroGPU Guest↔Host command stream protocol (host-side parser).
//!
//! ABI source of truth: `drivers/aerogpu/protocol/aerogpu_cmd.h`.
//!
//! To avoid drift, `aero-gpu` delegates ABI constants and basic decoding helpers to the canonical
//! `aero-protocol` crate.
//! The host consumes a byte slice containing:
//! - `aerogpu_cmd_stream_header`
//! - a sequence of command packets, each starting with `aerogpu_cmd_hdr`
//!
//! The parser is intentionally conservative:
//! - validates sizes and alignment
//! - skips unknown opcodes using `size_bytes`
//! - never performs unaligned reads into `repr(C, packed)` structs
//!
//! This allows the protocol to be consumed safely from guest-provided memory.

use core::{fmt, mem::size_of};

use aero_protocol::aerogpu::aerogpu_cmd as protocol;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuAbiError;

pub use protocol::{
    AerogpuCmdHdr as AeroGpuCmdHdr, AerogpuCmdOpcode as AeroGpuOpcode,
    AerogpuCmdStreamHeader as AeroGpuCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION, AEROGPU_MAX_RENDER_TARGETS,
};

use protocol::{decode_cmd_stream_header_le, AerogpuCmdDecodeError, AerogpuCmdStreamIter};

const INPUT_LAYOUT_BLOB_HEADER_MAGIC_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutBlobHeader, magic);
const INPUT_LAYOUT_BLOB_HEADER_VERSION_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutBlobHeader, version);
const INPUT_LAYOUT_BLOB_HEADER_ELEMENT_COUNT_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutBlobHeader, element_count);
const INPUT_LAYOUT_BLOB_HEADER_RESERVED0_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutBlobHeader, reserved0);

const INPUT_LAYOUT_ELEMENT_DXGI_SEMANTIC_NAME_HASH_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutElementDxgi, semantic_name_hash);
const INPUT_LAYOUT_ELEMENT_DXGI_SEMANTIC_INDEX_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutElementDxgi, semantic_index);
const INPUT_LAYOUT_ELEMENT_DXGI_FORMAT_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutElementDxgi, dxgi_format);
const INPUT_LAYOUT_ELEMENT_DXGI_INPUT_SLOT_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutElementDxgi, input_slot);
const INPUT_LAYOUT_ELEMENT_DXGI_ALIGNED_BYTE_OFFSET_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutElementDxgi, aligned_byte_offset);
const INPUT_LAYOUT_ELEMENT_DXGI_INPUT_SLOT_CLASS_OFFSET: usize =
    core::mem::offset_of!(protocol::AerogpuInputLayoutElementDxgi, input_slot_class);
const INPUT_LAYOUT_ELEMENT_DXGI_INSTANCE_DATA_STEP_RATE_OFFSET: usize = core::mem::offset_of!(
    protocol::AerogpuInputLayoutElementDxgi,
    instance_data_step_rate
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuBlendState {
    pub enable: u32,
    pub src_factor: u32,
    pub dst_factor: u32,
    pub blend_op: u32,
    pub color_write_mask: u8,
    pub src_factor_alpha: u32,
    pub dst_factor_alpha: u32,
    pub blend_op_alpha: u32,
    pub blend_constant_rgba_f32: [u32; 4],
    pub sample_mask: u32,
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
    pub flags: u32,
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
        if bytes.len() < protocol::AerogpuInputLayoutBlobHeader::SIZE_BYTES {
            return None;
        }
        Some(Self {
            magic: read_u32_le(
                &bytes[INPUT_LAYOUT_BLOB_HEADER_MAGIC_OFFSET
                    ..INPUT_LAYOUT_BLOB_HEADER_MAGIC_OFFSET + 4],
            ),
            version: read_u32_le(
                &bytes[INPUT_LAYOUT_BLOB_HEADER_VERSION_OFFSET
                    ..INPUT_LAYOUT_BLOB_HEADER_VERSION_OFFSET + 4],
            ),
            element_count: read_u32_le(
                &bytes[INPUT_LAYOUT_BLOB_HEADER_ELEMENT_COUNT_OFFSET
                    ..INPUT_LAYOUT_BLOB_HEADER_ELEMENT_COUNT_OFFSET + 4],
            ),
            reserved0: read_u32_le(
                &bytes[INPUT_LAYOUT_BLOB_HEADER_RESERVED0_OFFSET
                    ..INPUT_LAYOUT_BLOB_HEADER_RESERVED0_OFFSET + 4],
            ),
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
        if bytes.len() < protocol::AerogpuInputLayoutElementDxgi::SIZE_BYTES {
            return None;
        }
        Some(Self {
            semantic_name_hash: read_u32_le(
                &bytes[INPUT_LAYOUT_ELEMENT_DXGI_SEMANTIC_NAME_HASH_OFFSET
                    ..INPUT_LAYOUT_ELEMENT_DXGI_SEMANTIC_NAME_HASH_OFFSET + 4],
            ),
            semantic_index: read_u32_le(
                &bytes[INPUT_LAYOUT_ELEMENT_DXGI_SEMANTIC_INDEX_OFFSET
                    ..INPUT_LAYOUT_ELEMENT_DXGI_SEMANTIC_INDEX_OFFSET + 4],
            ),
            dxgi_format: read_u32_le(
                &bytes[INPUT_LAYOUT_ELEMENT_DXGI_FORMAT_OFFSET
                    ..INPUT_LAYOUT_ELEMENT_DXGI_FORMAT_OFFSET + 4],
            ),
            input_slot: read_u32_le(
                &bytes[INPUT_LAYOUT_ELEMENT_DXGI_INPUT_SLOT_OFFSET
                    ..INPUT_LAYOUT_ELEMENT_DXGI_INPUT_SLOT_OFFSET + 4],
            ),
            aligned_byte_offset: read_u32_le(
                &bytes[INPUT_LAYOUT_ELEMENT_DXGI_ALIGNED_BYTE_OFFSET_OFFSET
                    ..INPUT_LAYOUT_ELEMENT_DXGI_ALIGNED_BYTE_OFFSET_OFFSET + 4],
            ),
            input_slot_class: read_u32_le(
                &bytes[INPUT_LAYOUT_ELEMENT_DXGI_INPUT_SLOT_CLASS_OFFSET
                    ..INPUT_LAYOUT_ELEMENT_DXGI_INPUT_SLOT_CLASS_OFFSET + 4],
            ),
            instance_data_step_rate: read_u32_le(
                &bytes[INPUT_LAYOUT_ELEMENT_DXGI_INSTANCE_DATA_STEP_RATE_OFFSET
                    ..INPUT_LAYOUT_ELEMENT_DXGI_INSTANCE_DATA_STEP_RATE_OFFSET + 4],
            ),
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
    ReleaseSharedSurface {
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
    CopyBuffer {
        dst_buffer: u32,
        src_buffer: u32,
        dst_offset_bytes: u64,
        src_offset_bytes: u64,
        size_bytes: u64,
        flags: u32,
    },
    CopyTexture2d {
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
    },

    // Shaders
    CreateShaderDxbc {
        shader_handle: u32,
        stage: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// This is a raw ABI field used to disambiguate GS/HS/DS encoded with `stage=COMPUTE`.
        ///
        /// Higher layers should interpret it using the command stream header ABI minor:
        ///
        /// - Use `aero_protocol::aerogpu::aerogpu_cmd::decode_stage_ex_gated` /
        ///   `resolve_shader_stage_with_ex_gated` (stage_ex was introduced in ABI 1.3 / minor=3).
        /// - Extract `abi_minor` from `AeroGpuCmdStreamHeader.abi_version` using
        ///   `aero_protocol::aerogpu::aerogpu_pci::abi_minor`.
        stage_ex: u32,
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
        gs: u32,
        hs: u32,
        ds: u32,
    },
    SetShaderConstantsF {
        stage: u32,
        /// Reserved for ABI extension.
        ///
        /// When `stage == AEROGPU_SHADER_STAGE_COMPUTE`, this is interpreted as `stage_ex`
        /// (`AEROGPU_SHADER_STAGE_EX_*` / [`protocol::AerogpuShaderStageEx`]).
        reserved0: u32,
        start_register: u32,
        vec4_count: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
        data: &'a [u8],
    },
    SetShaderConstantsI {
        stage: u32,
        /// Reserved for ABI extension.
        ///
        /// When `stage == AEROGPU_SHADER_STAGE_COMPUTE`, this is interpreted as `stage_ex`
        /// (`AEROGPU_SHADER_STAGE_EX_*` / [`protocol::AerogpuShaderStageEx`]).
        reserved0: u32,
        start_register: u32,
        vec4_count: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
        data: &'a [u8],
    },
    SetShaderConstantsB {
        stage: u32,
        /// Reserved for ABI extension.
        ///
        /// When `stage == AEROGPU_SHADER_STAGE_COMPUTE`, this is interpreted as `stage_ex`
        /// (`AEROGPU_SHADER_STAGE_EX_*` / [`protocol::AerogpuShaderStageEx`]).
        reserved0: u32,
        start_register: u32,
        bool_count: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
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
        /// Reserved for ABI extension.
        ///
        /// When `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE`, this is interpreted as `stage_ex`
        /// (`AEROGPU_SHADER_STAGE_EX_*` / [`protocol::AerogpuShaderStageEx`]).
        reserved0: u32,
        slot: u32,
        texture: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
    },
    SetSamplerState {
        shader_stage: u32,
        slot: u32,
        state: u32,
        value: u32,
    },
    CreateSampler {
        sampler_handle: u32,
        filter: u32,
        address_u: u32,
        address_v: u32,
        address_w: u32,
    },
    DestroySampler {
        sampler_handle: u32,
    },
    SetSamplers {
        shader_stage: u32,
        /// Reserved for ABI extension.
        ///
        /// When `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE`, this is interpreted as `stage_ex`
        /// (`AEROGPU_SHADER_STAGE_EX_*` / [`protocol::AerogpuShaderStageEx`]).
        reserved0: u32,
        start_slot: u32,
        sampler_count: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
        handles_bytes: &'a [u8],
    },
    SetConstantBuffers {
        shader_stage: u32,
        /// Reserved for ABI extension.
        ///
        /// When `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE`, this is interpreted as `stage_ex`
        /// (`AEROGPU_SHADER_STAGE_EX_*` / [`protocol::AerogpuShaderStageEx`]).
        reserved0: u32,
        start_slot: u32,
        buffer_count: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
        bindings_bytes: &'a [u8],
    },
    SetShaderResourceBuffers {
        shader_stage: u32,
        start_slot: u32,
        buffer_count: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
        bindings_bytes: &'a [u8],
    },
    SetUnorderedAccessBuffers {
        shader_stage: u32,
        start_slot: u32,
        uav_count: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// See `CreateShaderDxbc.stage_ex` for details.
        stage_ex: u32,
        bindings_bytes: &'a [u8],
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
    Dispatch {
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
        /// Extended shader stage selector encoded in the packet's `reserved0` field.
        ///
        /// This is used by the D3D11 executor to disambiguate extended-stage compute dispatches
        /// (GS/HS/DS emulation) from legacy compute dispatches.
        ///
        /// Higher layers should interpret it using the command stream header ABI minor:
        ///
        /// - Use `aero_protocol::aerogpu::aerogpu_cmd::decode_stage_ex_gated` /
        ///   `resolve_shader_stage_with_ex_gated` (stage_ex was introduced in ABI 1.3 / minor=3).
        /// - Extract `abi_minor` from `AeroGpuCmdStreamHeader.abi_version` using
        ///   `aero_protocol::aerogpu::aerogpu_pci::abi_minor`.
        stage_ex: u32,
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
    UnsupportedAbiMajor { found: u16 },
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
            AeroGpuCmdStreamParseError::UnsupportedAbiMajor { found } => {
                write!(f, "unsupported ABI major version {found}")
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

#[derive(Clone)]
pub struct AeroGpuCmdStreamView<'a> {
    pub header: AeroGpuCmdStreamHeader,
    pub cmds: Vec<AeroGpuCmd<'a>>,
}

impl<'a> fmt::Debug for AeroGpuCmdStreamView<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header = (
            self.header.magic,
            self.header.abi_version,
            self.header.size_bytes,
            self.header.flags,
            self.header.reserved0,
            self.header.reserved1,
        );

        f.debug_struct("AeroGpuCmdStreamView")
            .field("header", &header)
            .field("cmds", &self.cmds)
            .finish()
    }
}

impl<'a> PartialEq for AeroGpuCmdStreamView<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.header.magic == other.header.magic
            && self.header.abi_version == other.header.abi_version
            && self.header.size_bytes == other.header.size_bytes
            && self.header.flags == other.header.flags
            && self.header.reserved0 == other.header.reserved0
            && self.header.reserved1 == other.header.reserved1
            && self.cmds == other.cmds
    }
}

impl<'a> Eq for AeroGpuCmdStreamView<'a> {}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn map_cmd_stream_header_error(
    bytes: &[u8],
    err: AerogpuCmdDecodeError,
) -> AeroGpuCmdStreamParseError {
    match err {
        AerogpuCmdDecodeError::BufferTooSmall => AeroGpuCmdStreamParseError::BufferTooSmall,
        AerogpuCmdDecodeError::BadMagic { found } => {
            AeroGpuCmdStreamParseError::InvalidMagic(found)
        }
        AerogpuCmdDecodeError::Abi(err) => match err {
            AerogpuAbiError::UnsupportedMajor { found } => {
                AeroGpuCmdStreamParseError::UnsupportedAbiMajor { found }
            }
        },
        AerogpuCmdDecodeError::BadSizeBytes { found } => {
            AeroGpuCmdStreamParseError::InvalidSizeBytes {
                size_bytes: found,
                buffer_len: bytes.len(),
            }
        }
        AerogpuCmdDecodeError::SizeNotAligned { found } => {
            // Stream header declares an invalid size_bytes (e.g. not 4-byte aligned).
            AeroGpuCmdStreamParseError::InvalidSizeBytes {
                size_bytes: found,
                buffer_len: bytes.len(),
            }
        }
        _ => AeroGpuCmdStreamParseError::BufferTooSmall,
    }
}

fn map_cmd_hdr_error(err: AerogpuCmdDecodeError) -> AeroGpuCmdStreamParseError {
    match err {
        AerogpuCmdDecodeError::BufferTooSmall => AeroGpuCmdStreamParseError::BufferTooSmall,
        AerogpuCmdDecodeError::BadSizeBytes { found } => {
            AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(found)
        }
        AerogpuCmdDecodeError::SizeNotAligned { found } => {
            AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(found)
        }
        _ => AeroGpuCmdStreamParseError::BufferTooSmall,
    }
}

fn read_packed_prefix<T: Copy>(bytes: &[u8]) -> Result<T, AeroGpuCmdStreamParseError> {
    if bytes.len() < size_of::<T>() {
        return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
    }

    // SAFETY: Bounds checked above and `read_unaligned` avoids alignment requirements.
    Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

pub fn parse_cmd_stream(
    bytes: &[u8],
) -> Result<AeroGpuCmdStreamView<'_>, AeroGpuCmdStreamParseError> {
    let header = decode_cmd_stream_header_le(bytes)
        .map_err(|err| map_cmd_stream_header_error(bytes, err))?;

    let size_bytes_usize = header.size_bytes as usize;
    if size_bytes_usize < AeroGpuCmdStreamHeader::SIZE_BYTES || size_bytes_usize > bytes.len() {
        return Err(AeroGpuCmdStreamParseError::InvalidSizeBytes {
            size_bytes: header.size_bytes,
            buffer_len: bytes.len(),
        });
    }

    let mut cmds = Vec::new();
    let mut offset = AeroGpuCmdStreamHeader::SIZE_BYTES;
    let iter = AerogpuCmdStreamIter::new(&bytes[..size_bytes_usize])
        .map_err(|err| map_cmd_stream_header_error(bytes, err))?;
    for pkt in iter {
        let pkt = pkt.map_err(map_cmd_hdr_error)?;
        let cmd_hdr = pkt.hdr;
        let cmd_size_usize = cmd_hdr.size_bytes as usize;
        let end = offset.checked_add(cmd_size_usize).ok_or(
            AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(cmd_hdr.size_bytes),
        )?;
        if end > size_bytes_usize {
            return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
        }

        let packet = &bytes[offset..end];
        let payload = pkt.payload;

        let cmd = match AeroGpuOpcode::from_u32(cmd_hdr.opcode) {
            Some(AeroGpuOpcode::Nop) => AeroGpuCmd::Nop,
            Some(AeroGpuOpcode::DebugMarker) => AeroGpuCmd::DebugMarker { bytes: payload },

            Some(AeroGpuOpcode::CreateBuffer) => {
                let cmd: protocol::AerogpuCmdCreateBuffer = read_packed_prefix(packet)?;
                AeroGpuCmd::CreateBuffer {
                    buffer_handle: u32::from_le(cmd.buffer_handle),
                    usage_flags: u32::from_le(cmd.usage_flags),
                    size_bytes: u64::from_le(cmd.size_bytes),
                    backing_alloc_id: u32::from_le(cmd.backing_alloc_id),
                    backing_offset_bytes: u32::from_le(cmd.backing_offset_bytes),
                }
            }
            Some(AeroGpuOpcode::CreateTexture2d) => {
                let cmd: protocol::AerogpuCmdCreateTexture2d = read_packed_prefix(packet)?;
                AeroGpuCmd::CreateTexture2d {
                    texture_handle: u32::from_le(cmd.texture_handle),
                    usage_flags: u32::from_le(cmd.usage_flags),
                    format: u32::from_le(cmd.format),
                    width: u32::from_le(cmd.width),
                    height: u32::from_le(cmd.height),
                    mip_levels: u32::from_le(cmd.mip_levels),
                    array_layers: u32::from_le(cmd.array_layers),
                    row_pitch_bytes: u32::from_le(cmd.row_pitch_bytes),
                    backing_alloc_id: u32::from_le(cmd.backing_alloc_id),
                    backing_offset_bytes: u32::from_le(cmd.backing_offset_bytes),
                }
            }
            Some(AeroGpuOpcode::DestroyResource) => {
                let cmd: protocol::AerogpuCmdDestroyResource = read_packed_prefix(packet)?;
                AeroGpuCmd::DestroyResource {
                    resource_handle: u32::from_le(cmd.resource_handle),
                }
            }
            Some(AeroGpuOpcode::ResourceDirtyRange) => {
                let cmd: protocol::AerogpuCmdResourceDirtyRange = read_packed_prefix(packet)?;
                AeroGpuCmd::ResourceDirtyRange {
                    resource_handle: u32::from_le(cmd.resource_handle),
                    offset_bytes: u64::from_le(cmd.offset_bytes),
                    size_bytes: u64::from_le(cmd.size_bytes),
                }
            }
            Some(AeroGpuOpcode::UploadResource) => {
                let (cmd, data) = protocol::decode_cmd_upload_resource_payload_le(packet)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::UploadResource {
                    resource_handle: cmd.resource_handle,
                    offset_bytes: cmd.offset_bytes,
                    size_bytes: cmd.size_bytes,
                    data,
                }
            }
            Some(AeroGpuOpcode::CopyBuffer) => {
                let cmd: protocol::AerogpuCmdCopyBuffer = read_packed_prefix(packet)?;
                AeroGpuCmd::CopyBuffer {
                    dst_buffer: u32::from_le(cmd.dst_buffer),
                    src_buffer: u32::from_le(cmd.src_buffer),
                    dst_offset_bytes: u64::from_le(cmd.dst_offset_bytes),
                    src_offset_bytes: u64::from_le(cmd.src_offset_bytes),
                    size_bytes: u64::from_le(cmd.size_bytes),
                    flags: u32::from_le(cmd.flags),
                }
            }
            Some(AeroGpuOpcode::CopyTexture2d) => {
                let cmd: protocol::AerogpuCmdCopyTexture2d = read_packed_prefix(packet)?;
                AeroGpuCmd::CopyTexture2d {
                    dst_texture: u32::from_le(cmd.dst_texture),
                    src_texture: u32::from_le(cmd.src_texture),
                    dst_mip_level: u32::from_le(cmd.dst_mip_level),
                    dst_array_layer: u32::from_le(cmd.dst_array_layer),
                    src_mip_level: u32::from_le(cmd.src_mip_level),
                    src_array_layer: u32::from_le(cmd.src_array_layer),
                    dst_x: u32::from_le(cmd.dst_x),
                    dst_y: u32::from_le(cmd.dst_y),
                    src_x: u32::from_le(cmd.src_x),
                    src_y: u32::from_le(cmd.src_y),
                    width: u32::from_le(cmd.width),
                    height: u32::from_le(cmd.height),
                    flags: u32::from_le(cmd.flags),
                }
            }

            Some(AeroGpuOpcode::CreateShaderDxbc) => {
                let (cmd, dxbc_bytes) = protocol::decode_cmd_create_shader_dxbc_payload_le(packet)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::CreateShaderDxbc {
                    shader_handle: cmd.shader_handle,
                    stage: cmd.stage,
                    stage_ex: cmd.reserved0,
                    dxbc_size_bytes: cmd.dxbc_size_bytes,
                    dxbc_bytes,
                }
            }
            Some(AeroGpuOpcode::DestroyShader) => {
                let cmd: protocol::AerogpuCmdDestroyShader = read_packed_prefix(packet)?;
                AeroGpuCmd::DestroyShader {
                    shader_handle: u32::from_le(cmd.shader_handle),
                }
            }
            Some(AeroGpuOpcode::BindShaders) => {
                let (cmd, ex) = protocol::decode_cmd_bind_shaders_payload_le(packet)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let (gs, hs, ds) = match ex {
                    Some(ex) => (ex.gs, ex.hs, ex.ds),
                    None => (cmd.gs(), 0, 0),
                };
                AeroGpuCmd::BindShaders {
                    vs: cmd.vs,
                    ps: cmd.ps,
                    cs: cmd.cs,
                    gs,
                    hs,
                    ds,
                }
            }
            Some(AeroGpuOpcode::SetShaderConstantsF) => {
                let cmd: protocol::AerogpuCmdSetShaderConstantsF = read_packed_prefix(packet)?;
                let vec4_count = u32::from_le(cmd.vec4_count);
                let stage_ex = u32::from_le(cmd.reserved0);
                let data_len = (vec4_count as usize)
                    .checked_mul(16)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data_start = protocol::AerogpuCmdSetShaderConstantsF::SIZE_BYTES;
                let data_end = data_start
                    .checked_add(data_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data = packet
                    .get(data_start..data_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetShaderConstantsF {
                    stage: u32::from_le(cmd.stage),
                    reserved0: u32::from_le(cmd.reserved0),
                    start_register: u32::from_le(cmd.start_register),
                    vec4_count,
                    stage_ex,
                    data,
                }
            }
            Some(AeroGpuOpcode::SetShaderConstantsI) => {
                let cmd: protocol::AerogpuCmdSetShaderConstantsI = read_packed_prefix(packet)?;
                let vec4_count = u32::from_le(cmd.vec4_count);
                let stage_ex = u32::from_le(cmd.reserved0);
                let data_len = (vec4_count as usize)
                    .checked_mul(16)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data_start = protocol::AerogpuCmdSetShaderConstantsI::SIZE_BYTES;
                let data_end = data_start
                    .checked_add(data_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data = packet
                    .get(data_start..data_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetShaderConstantsI {
                    stage: u32::from_le(cmd.stage),
                    reserved0: u32::from_le(cmd.reserved0),
                    start_register: u32::from_le(cmd.start_register),
                    vec4_count,
                    stage_ex,
                    data,
                }
            }
            Some(AeroGpuOpcode::SetShaderConstantsB) => {
                let cmd: protocol::AerogpuCmdSetShaderConstantsB = read_packed_prefix(packet)?;
                let bool_count = u32::from_le(cmd.bool_count);
                let stage_ex = u32::from_le(cmd.reserved0);
                let data_len = (bool_count as usize)
                    .checked_mul(4)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data_start = protocol::AerogpuCmdSetShaderConstantsB::SIZE_BYTES;
                let data_end = data_start
                    .checked_add(data_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let data = packet
                    .get(data_start..data_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetShaderConstantsB {
                    stage: u32::from_le(cmd.stage),
                    reserved0: u32::from_le(cmd.reserved0),
                    start_register: u32::from_le(cmd.start_register),
                    bool_count,
                    stage_ex,
                    data,
                }
            }

            Some(AeroGpuOpcode::CreateInputLayout) => {
                let (cmd, blob_bytes) = protocol::decode_cmd_create_input_layout_blob_le(packet)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::CreateInputLayout {
                    input_layout_handle: cmd.input_layout_handle,
                    blob_size_bytes: cmd.blob_size_bytes,
                    blob_bytes,
                }
            }
            Some(AeroGpuOpcode::DestroyInputLayout) => {
                let cmd: protocol::AerogpuCmdDestroyInputLayout = read_packed_prefix(packet)?;
                AeroGpuCmd::DestroyInputLayout {
                    input_layout_handle: u32::from_le(cmd.input_layout_handle),
                }
            }
            Some(AeroGpuOpcode::SetInputLayout) => {
                let cmd: protocol::AerogpuCmdSetInputLayout = read_packed_prefix(packet)?;
                AeroGpuCmd::SetInputLayout {
                    input_layout_handle: u32::from_le(cmd.input_layout_handle),
                }
            }

            Some(AeroGpuOpcode::SetBlendState) => {
                // `SET_BLEND_STATE` was extended over time. Accept the legacy 28-byte packet and
                // default missing fields (alpha params, blend constant, sample mask) so older
                // guests can still be parsed.
                if packet.len() < 28 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }

                let enable = read_u32_le(&packet[8..12]);
                let src_factor = read_u32_le(&packet[12..16]);
                let dst_factor = read_u32_le(&packet[16..20]);
                let blend_op = read_u32_le(&packet[20..24]);
                let color_write_mask = packet[24];

                let src_factor_alpha = if packet.len() >= 32 {
                    read_u32_le(&packet[28..32])
                } else {
                    src_factor
                };
                let dst_factor_alpha = if packet.len() >= 36 {
                    read_u32_le(&packet[32..36])
                } else {
                    dst_factor
                };
                let blend_op_alpha = if packet.len() >= 40 {
                    read_u32_le(&packet[36..40])
                } else {
                    blend_op
                };

                let mut blend_constant_rgba_f32 = [1.0f32.to_bits(); 4];
                if packet.len() >= 44 {
                    blend_constant_rgba_f32[0] = read_u32_le(&packet[40..44]);
                }
                if packet.len() >= 48 {
                    blend_constant_rgba_f32[1] = read_u32_le(&packet[44..48]);
                }
                if packet.len() >= 52 {
                    blend_constant_rgba_f32[2] = read_u32_le(&packet[48..52]);
                }
                if packet.len() >= 56 {
                    blend_constant_rgba_f32[3] = read_u32_le(&packet[52..56]);
                }
                let sample_mask = if packet.len() >= 60 {
                    read_u32_le(&packet[56..60])
                } else {
                    0xFFFF_FFFF
                };

                AeroGpuCmd::SetBlendState {
                    state: AeroGpuBlendState {
                        enable,
                        src_factor,
                        dst_factor,
                        blend_op,
                        color_write_mask,
                        src_factor_alpha,
                        dst_factor_alpha,
                        blend_op_alpha,
                        blend_constant_rgba_f32,
                        sample_mask,
                    },
                }
            }
            Some(AeroGpuOpcode::SetDepthStencilState) => {
                let cmd: protocol::AerogpuCmdSetDepthStencilState = read_packed_prefix(packet)?;
                let state = cmd.state;
                AeroGpuCmd::SetDepthStencilState {
                    state: AeroGpuDepthStencilState {
                        depth_enable: u32::from_le(state.depth_enable),
                        depth_write_enable: u32::from_le(state.depth_write_enable),
                        depth_func: u32::from_le(state.depth_func),
                        stencil_enable: u32::from_le(state.stencil_enable),
                        stencil_read_mask: state.stencil_read_mask,
                        stencil_write_mask: state.stencil_write_mask,
                    },
                }
            }
            Some(AeroGpuOpcode::SetRasterizerState) => {
                let cmd: protocol::AerogpuCmdSetRasterizerState = read_packed_prefix(packet)?;
                let state = cmd.state;
                AeroGpuCmd::SetRasterizerState {
                    state: AeroGpuRasterizerState {
                        fill_mode: u32::from_le(state.fill_mode),
                        cull_mode: u32::from_le(state.cull_mode),
                        front_ccw: u32::from_le(state.front_ccw),
                        scissor_enable: u32::from_le(state.scissor_enable),
                        depth_bias: i32::from_le(state.depth_bias),
                        flags: u32::from_le(state.flags),
                    },
                }
            }

            Some(AeroGpuOpcode::SetRenderTargets) => {
                let cmd: protocol::AerogpuCmdSetRenderTargets = read_packed_prefix(packet)?;
                AeroGpuCmd::SetRenderTargets {
                    color_count: u32::from_le(cmd.color_count),
                    depth_stencil: u32::from_le(cmd.depth_stencil),
                    colors: cmd.colors.map(u32::from_le),
                }
            }
            Some(AeroGpuOpcode::SetViewport) => {
                let cmd: protocol::AerogpuCmdSetViewport = read_packed_prefix(packet)?;
                AeroGpuCmd::SetViewport {
                    x_f32: u32::from_le(cmd.x_f32),
                    y_f32: u32::from_le(cmd.y_f32),
                    width_f32: u32::from_le(cmd.width_f32),
                    height_f32: u32::from_le(cmd.height_f32),
                    min_depth_f32: u32::from_le(cmd.min_depth_f32),
                    max_depth_f32: u32::from_le(cmd.max_depth_f32),
                }
            }
            Some(AeroGpuOpcode::SetScissor) => {
                let cmd: protocol::AerogpuCmdSetScissor = read_packed_prefix(packet)?;
                AeroGpuCmd::SetScissor {
                    x: i32::from_le(cmd.x),
                    y: i32::from_le(cmd.y),
                    width: i32::from_le(cmd.width),
                    height: i32::from_le(cmd.height),
                }
            }

            Some(AeroGpuOpcode::SetVertexBuffers) => {
                let (cmd, bindings) = protocol::decode_cmd_set_vertex_buffers_bindings_le(packet)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_start = size_of::<protocol::AerogpuCmdSetVertexBuffers>();
                let bindings_len =
                    bindings.len() * protocol::AerogpuVertexBufferBinding::SIZE_BYTES;
                let bindings_end = bindings_start
                    .checked_add(bindings_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_bytes = packet
                    .get(bindings_start..bindings_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetVertexBuffers {
                    start_slot: cmd.start_slot,
                    buffer_count: cmd.buffer_count,
                    bindings_bytes,
                }
            }
            Some(AeroGpuOpcode::SetIndexBuffer) => {
                let cmd: protocol::AerogpuCmdSetIndexBuffer = read_packed_prefix(packet)?;
                AeroGpuCmd::SetIndexBuffer {
                    buffer: u32::from_le(cmd.buffer),
                    format: u32::from_le(cmd.format),
                    offset_bytes: u32::from_le(cmd.offset_bytes),
                }
            }
            Some(AeroGpuOpcode::SetPrimitiveTopology) => {
                let cmd: protocol::AerogpuCmdSetPrimitiveTopology = read_packed_prefix(packet)?;
                AeroGpuCmd::SetPrimitiveTopology {
                    topology: u32::from_le(cmd.topology),
                }
            }
            Some(AeroGpuOpcode::SetTexture) => {
                let cmd: protocol::AerogpuCmdSetTexture = read_packed_prefix(packet)?;
                AeroGpuCmd::SetTexture {
                    shader_stage: u32::from_le(cmd.shader_stage),
                    reserved0: u32::from_le(cmd.reserved0),
                    slot: u32::from_le(cmd.slot),
                    texture: u32::from_le(cmd.texture),
                    stage_ex: u32::from_le(cmd.reserved0),
                }
            }
            Some(AeroGpuOpcode::SetSamplerState) => {
                let cmd: protocol::AerogpuCmdSetSamplerState = read_packed_prefix(packet)?;
                AeroGpuCmd::SetSamplerState {
                    shader_stage: u32::from_le(cmd.shader_stage),
                    slot: u32::from_le(cmd.slot),
                    state: u32::from_le(cmd.state),
                    value: u32::from_le(cmd.value),
                }
            }
            Some(AeroGpuOpcode::CreateSampler) => {
                let cmd: protocol::AerogpuCmdCreateSampler = read_packed_prefix(packet)?;
                AeroGpuCmd::CreateSampler {
                    sampler_handle: u32::from_le(cmd.sampler_handle),
                    filter: u32::from_le(cmd.filter),
                    address_u: u32::from_le(cmd.address_u),
                    address_v: u32::from_le(cmd.address_v),
                    address_w: u32::from_le(cmd.address_w),
                }
            }
            Some(AeroGpuOpcode::DestroySampler) => {
                let cmd: protocol::AerogpuCmdDestroySampler = read_packed_prefix(packet)?;
                AeroGpuCmd::DestroySampler {
                    sampler_handle: u32::from_le(cmd.sampler_handle),
                }
            }
            Some(AeroGpuOpcode::SetSamplers) => {
                let cmd: protocol::AerogpuCmdSetSamplers = read_packed_prefix(packet)?;
                let handles_start = size_of::<protocol::AerogpuCmdSetSamplers>();
                let sampler_count = u32::from_le(cmd.sampler_count);
                let stage_ex = u32::from_le(cmd.reserved0);
                let count = usize::try_from(sampler_count)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let handles_len = count
                    .checked_mul(size_of::<u32>())
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let handles_end = handles_start
                    .checked_add(handles_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let handles_bytes = packet
                    .get(handles_start..handles_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetSamplers {
                    shader_stage: u32::from_le(cmd.shader_stage),
                    reserved0: u32::from_le(cmd.reserved0),
                    start_slot: u32::from_le(cmd.start_slot),
                    sampler_count,
                    stage_ex,
                    handles_bytes,
                }
            }
            Some(AeroGpuOpcode::SetConstantBuffers) => {
                let cmd: protocol::AerogpuCmdSetConstantBuffers = read_packed_prefix(packet)?;
                let bindings_start = size_of::<protocol::AerogpuCmdSetConstantBuffers>();
                let buffer_count = u32::from_le(cmd.buffer_count);
                let stage_ex = u32::from_le(cmd.reserved0);
                let count = usize::try_from(buffer_count)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_len = count
                    .checked_mul(size_of::<protocol::AerogpuConstantBufferBinding>())
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_end = bindings_start
                    .checked_add(bindings_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_bytes = packet
                    .get(bindings_start..bindings_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetConstantBuffers {
                    shader_stage: u32::from_le(cmd.shader_stage),
                    reserved0: u32::from_le(cmd.reserved0),
                    start_slot: u32::from_le(cmd.start_slot),
                    buffer_count,
                    stage_ex,
                    bindings_bytes,
                }
            }
            Some(AeroGpuOpcode::SetShaderResourceBuffers) => {
                let cmd: protocol::AerogpuCmdSetShaderResourceBuffers = read_packed_prefix(packet)?;
                let bindings_start = size_of::<protocol::AerogpuCmdSetShaderResourceBuffers>();
                let buffer_count = u32::from_le(cmd.buffer_count);
                let stage_ex = u32::from_le(cmd.reserved0);
                let count = usize::try_from(buffer_count)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_len = count
                    .checked_mul(size_of::<protocol::AerogpuShaderResourceBufferBinding>())
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_end = bindings_start
                    .checked_add(bindings_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_bytes = packet
                    .get(bindings_start..bindings_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetShaderResourceBuffers {
                    shader_stage: u32::from_le(cmd.shader_stage),
                    start_slot: u32::from_le(cmd.start_slot),
                    buffer_count,
                    stage_ex,
                    bindings_bytes,
                }
            }
            Some(AeroGpuOpcode::SetUnorderedAccessBuffers) => {
                let cmd: protocol::AerogpuCmdSetUnorderedAccessBuffers =
                    read_packed_prefix(packet)?;
                let bindings_start = size_of::<protocol::AerogpuCmdSetUnorderedAccessBuffers>();
                let uav_count = u32::from_le(cmd.uav_count);
                let stage_ex = u32::from_le(cmd.reserved0);
                let count = usize::try_from(uav_count)
                    .map_err(|_| AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_len = count
                    .checked_mul(size_of::<protocol::AerogpuUnorderedAccessBufferBinding>())
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_end = bindings_start
                    .checked_add(bindings_len)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                let bindings_bytes = packet
                    .get(bindings_start..bindings_end)
                    .ok_or(AeroGpuCmdStreamParseError::BufferTooSmall)?;
                AeroGpuCmd::SetUnorderedAccessBuffers {
                    shader_stage: u32::from_le(cmd.shader_stage),
                    start_slot: u32::from_le(cmd.start_slot),
                    uav_count,
                    stage_ex,
                    bindings_bytes,
                }
            }
            Some(AeroGpuOpcode::SetRenderState) => {
                let cmd: protocol::AerogpuCmdSetRenderState = read_packed_prefix(packet)?;
                AeroGpuCmd::SetRenderState {
                    state: u32::from_le(cmd.state),
                    value: u32::from_le(cmd.value),
                }
            }

            Some(AeroGpuOpcode::Clear) => {
                let cmd: protocol::AerogpuCmdClear = read_packed_prefix(packet)?;
                AeroGpuCmd::Clear {
                    flags: u32::from_le(cmd.flags),
                    color_rgba_f32: cmd.color_rgba_f32.map(u32::from_le),
                    depth_f32: u32::from_le(cmd.depth_f32),
                    stencil: u32::from_le(cmd.stencil),
                }
            }
            Some(AeroGpuOpcode::Draw) => {
                let cmd: protocol::AerogpuCmdDraw = read_packed_prefix(packet)?;
                AeroGpuCmd::Draw {
                    vertex_count: u32::from_le(cmd.vertex_count),
                    instance_count: u32::from_le(cmd.instance_count),
                    first_vertex: u32::from_le(cmd.first_vertex),
                    first_instance: u32::from_le(cmd.first_instance),
                }
            }
            Some(AeroGpuOpcode::DrawIndexed) => {
                let cmd: protocol::AerogpuCmdDrawIndexed = read_packed_prefix(packet)?;
                AeroGpuCmd::DrawIndexed {
                    index_count: u32::from_le(cmd.index_count),
                    instance_count: u32::from_le(cmd.instance_count),
                    first_index: u32::from_le(cmd.first_index),
                    base_vertex: i32::from_le(cmd.base_vertex),
                    first_instance: u32::from_le(cmd.first_instance),
                }
            }
            Some(AeroGpuOpcode::Dispatch) => {
                let cmd: protocol::AerogpuCmdDispatch = read_packed_prefix(packet)?;
                AeroGpuCmd::Dispatch {
                    group_count_x: u32::from_le(cmd.group_count_x),
                    group_count_y: u32::from_le(cmd.group_count_y),
                    group_count_z: u32::from_le(cmd.group_count_z),
                    stage_ex: u32::from_le(cmd.reserved0),
                }
            }

            Some(AeroGpuOpcode::Present) => {
                let cmd: protocol::AerogpuCmdPresent = read_packed_prefix(packet)?;
                AeroGpuCmd::Present {
                    scanout_id: u32::from_le(cmd.scanout_id),
                    flags: u32::from_le(cmd.flags),
                }
            }
            Some(AeroGpuOpcode::PresentEx) => {
                let cmd: protocol::AerogpuCmdPresentEx = read_packed_prefix(packet)?;
                AeroGpuCmd::PresentEx {
                    scanout_id: u32::from_le(cmd.scanout_id),
                    flags: u32::from_le(cmd.flags),
                    d3d9_present_flags: u32::from_le(cmd.d3d9_present_flags),
                }
            }

            Some(AeroGpuOpcode::ExportSharedSurface) => {
                let cmd: protocol::AerogpuCmdExportSharedSurface = read_packed_prefix(packet)?;
                AeroGpuCmd::ExportSharedSurface {
                    resource_handle: u32::from_le(cmd.resource_handle),
                    share_token: u64::from_le(cmd.share_token),
                }
            }
            Some(AeroGpuOpcode::ImportSharedSurface) => {
                let cmd: protocol::AerogpuCmdImportSharedSurface = read_packed_prefix(packet)?;
                AeroGpuCmd::ImportSharedSurface {
                    out_resource_handle: u32::from_le(cmd.out_resource_handle),
                    share_token: u64::from_le(cmd.share_token),
                }
            }
            Some(AeroGpuOpcode::ReleaseSharedSurface) => {
                let cmd: protocol::AerogpuCmdReleaseSharedSurface = read_packed_prefix(packet)?;
                AeroGpuCmd::ReleaseSharedSurface {
                    share_token: u64::from_le(cmd.share_token),
                }
            }

            Some(AeroGpuOpcode::Flush) => {
                let _: protocol::AerogpuCmdFlush = read_packed_prefix(packet)?;
                AeroGpuCmd::Flush
            }

            // Forward-compat: if `aero-protocol` learns about a new opcode but `aero-gpu` doesn't yet
            // have a typed decoder for it, treat it as unknown and allow higher layers to skip it.
            _ => AeroGpuCmd::Unknown {
                opcode: cmd_hdr.opcode,
                payload,
            },
        };

        // Avoid process aborts from OOM when parsing guest-controlled streams.
        if cmds.try_reserve(1).is_err() {
            return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
        }
        cmds.push(cmd);
        offset = end;
    }

    Ok(AeroGpuCmdStreamView { header, cmds })
}
