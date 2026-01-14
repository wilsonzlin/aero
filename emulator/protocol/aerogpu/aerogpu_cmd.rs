//! AeroGPU command stream layouts.
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_cmd.h`.
//! ABI is validated by `emulator/protocol/tests/aerogpu_abi.rs` and `emulator/protocol/tests/aerogpu_abi.test.ts`.

use super::aerogpu_pci::{parse_and_validate_abi_version_u32, AerogpuAbiError};
use core::fmt;

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
    CopyBuffer = 0x105,
    CopyTexture2d = 0x106,
    CreateTextureView = 0x107,
    DestroyTextureView = 0x108,

    CreateShaderDxbc = 0x200,
    DestroyShader = 0x201,
    BindShaders = 0x202,
    SetShaderConstantsF = 0x203,
    CreateInputLayout = 0x204,
    DestroyInputLayout = 0x205,
    SetInputLayout = 0x206,
    SetShaderConstantsI = 0x207,
    SetShaderConstantsB = 0x208,

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

    CreateSampler = 0x520,
    DestroySampler = 0x521,
    SetSamplers = 0x522,
    SetConstantBuffers = 0x523,
    SetShaderResourceBuffers = 0x524,
    SetUnorderedAccessBuffers = 0x525,

    Clear = 0x600,
    Draw = 0x601,
    DrawIndexed = 0x602,
    Dispatch = 0x603,

    Present = 0x700,
    PresentEx = 0x701,

    ExportSharedSurface = 0x710,
    ImportSharedSurface = 0x711,
    ReleaseSharedSurface = 0x712,

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
            0x105 => Some(Self::CopyBuffer),
            0x106 => Some(Self::CopyTexture2d),
            0x107 => Some(Self::CreateTextureView),
            0x108 => Some(Self::DestroyTextureView),
            0x200 => Some(Self::CreateShaderDxbc),
            0x201 => Some(Self::DestroyShader),
            0x202 => Some(Self::BindShaders),
            0x203 => Some(Self::SetShaderConstantsF),
            0x204 => Some(Self::CreateInputLayout),
            0x205 => Some(Self::DestroyInputLayout),
            0x206 => Some(Self::SetInputLayout),
            0x207 => Some(Self::SetShaderConstantsI),
            0x208 => Some(Self::SetShaderConstantsB),
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
            0x520 => Some(Self::CreateSampler),
            0x521 => Some(Self::DestroySampler),
            0x522 => Some(Self::SetSamplers),
            0x523 => Some(Self::SetConstantBuffers),
            0x524 => Some(Self::SetShaderResourceBuffers),
            0x525 => Some(Self::SetUnorderedAccessBuffers),
            0x600 => Some(Self::Clear),
            0x601 => Some(Self::Draw),
            0x602 => Some(Self::DrawIndexed),
            0x603 => Some(Self::Dispatch),
            0x700 => Some(Self::Present),
            0x701 => Some(Self::PresentEx),
            0x710 => Some(Self::ExportSharedSurface),
            0x711 => Some(Self::ImportSharedSurface),
            0x712 => Some(Self::ReleaseSharedSurface),
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
    /// D3D11 geometry shader stage.
    ///
    /// Note: WebGPU does not expose geometry shaders, but AeroGPU still carries the stage to
    /// allow D3D11 command streams to be forwarded without dropping stage-local binding state.
    Geometry = 3,
}

impl AerogpuShaderStage {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Vertex),
            1 => Some(Self::Pixel),
            2 => Some(Self::Compute),
            3 => Some(Self::Geometry),
            _ => None,
        }
    }
}
/// Extended shader stage encoding (`stage_ex`).
///
/// Some packets contain a `shader_stage` (or `stage`) field whose base enum supports VS/PS/CS (+ GS).
/// To represent additional D3D10+ stages (HS/DS) without changing packet layouts, when
/// `shader_stage == AerogpuShaderStage::Compute` the packet's `reserved0` field is repurposed as a
/// `stage_ex` override. If `shader_stage != Compute`, `reserved0` MUST be 0 and is ignored.
///
/// Canonical rules:
/// - `reserved0 == 0` means "no stage_ex override" and MUST be interpreted as the legacy Compute
///   stage (older guests always wrote 0 into reserved fields).
/// - Non-zero `reserved0` values are interpreted as [`AerogpuShaderStageEx`].
///
/// Note: Geometry is also representable directly as [`AerogpuShaderStage::Geometry`] in the legacy
/// stage enum; `stage_ex` is primarily needed for HS/DS (and as a compatibility encoding for GS).
///
/// Numeric values intentionally match the D3D DXBC "program type" numbers used in the shader
/// version token: Pixel=0, Vertex=1, Geometry=2, Hull=3, Domain=4, Compute=5.
///
/// `stage_ex` can only represent the non-legacy stages because:
/// - `reserved0 == 0` is reserved for "no override" (legacy Compute), so `stage_ex` cannot encode
///   Pixel (0), and
/// - Vertex (1) must be encoded via the legacy `shader_stage = Vertex` for clarity; `reserved0 == 1`
///   is intentionally invalid and must be rejected by decoders.
///
/// [`AerogpuShaderStageEx::Compute`] (5) is accepted by [`resolve_stage`] and treated the same as
/// "no override" (Compute). Writers should emit 0 for Compute to preserve legacy packet semantics.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuShaderStageEx {
    /// 0 = no stage_ex override (legacy Compute).
    None = 0,
    Geometry = 2,
    Hull = 3,
    Domain = 4,
    /// Optional alias for Compute (see docs above).
    Compute = 5,
}

impl AerogpuShaderStageEx {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            2 => Some(Self::Geometry),
            3 => Some(Self::Hull),
            4 => Some(Self::Domain),
            5 => Some(Self::Compute),
            _ => None,
        }
    }
}

/// Decode the extended shader stage ("stage_ex") from a `(shader_stage, reserved0)` pair.
///
/// The "stage_ex" ABI extension overloads the `reserved0` field of certain commands that already
/// include a legacy `shader_stage`/`stage` field (e.g. `SET_TEXTURE`, `SET_SAMPLERS`,
/// `SET_CONSTANT_BUFFERS`, `SET_SHADER_CONSTANTS_F`, `SET_SHADER_CONSTANTS_I`, `SET_SHADER_CONSTANTS_B`,
/// `SET_SHADER_RESOURCE_BUFFERS`, `SET_UNORDERED_ACCESS_BUFFERS`, `CREATE_SHADER_DXBC`).
///
/// The overload is only active when `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE`.
pub fn decode_stage_ex(shader_stage: u32, reserved0: u32) -> Option<AerogpuShaderStageEx> {
    if shader_stage == AerogpuShaderStage::Compute as u32 {
        AerogpuShaderStageEx::from_u32(reserved0)
    } else {
        None
    }
}

/// ABI minor version that introduced the `stage_ex` encoding in `reserved0`.
///
/// Older guests (command stream ABI minor < this value) may not reliably zero `reserved0`, so
/// hosts must ignore it to avoid misinterpreting garbage as a `stage_ex` selector.
pub const AEROGPU_STAGE_EX_MIN_ABI_MINOR: u16 = 3;

/// Decode an extended shader stage encoded in a packet's `reserved0` field, gated by ABI minor.
///
/// For command streams older than [`AEROGPU_STAGE_EX_MIN_ABI_MINOR`], `reserved0` is ignored when
/// `shader_stage == Compute` to preserve legacy behavior.
pub fn decode_stage_ex_gated(
    abi_minor: u16,
    shader_stage: u32,
    reserved0: u32,
) -> Option<AerogpuShaderStageEx> {
    if abi_minor < AEROGPU_STAGE_EX_MIN_ABI_MINOR
        && shader_stage == AerogpuShaderStage::Compute as u32
    {
        return decode_stage_ex(shader_stage, 0);
    }
    decode_stage_ex(shader_stage, reserved0)
}

/// Encode the extended shader stage ("stage_ex") into `(shader_stage, reserved0)`.
///
/// The returned `shader_stage` is always `AEROGPU_SHADER_STAGE_COMPUTE`.
///
/// Note: Compute is canonicalized to legacy encoding (`reserved0 == 0`) to preserve backwards
/// compatibility with older guests.
pub fn encode_stage_ex(stage_ex: AerogpuShaderStageEx) -> (u32, u32) {
    let reserved0 = match stage_ex {
        AerogpuShaderStageEx::None => 0,
        // Canonicalize Compute to legacy encoding (`reserved0==0`).
        AerogpuShaderStageEx::Compute => 0,
        _ => stage_ex as u32,
    };
    (AerogpuShaderStage::Compute as u32, reserved0)
}

/// Effective shader stage after applying `stage_ex` override rules.
///
/// Numeric values match the D3D DXBC "program type" numbers (Pixel=0..Compute=5).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuD3dShaderStage {
    Pixel = 0,
    Vertex = 1,
    Geometry = 2,
    Hull = 3,
    Domain = 4,
    Compute = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuStageResolveError {
    UnknownShaderStage { shader_stage: u32 },
    UnknownStageEx { stage_ex: u32 },
    InvalidStageEx { stage_ex: u32 },
}

impl fmt::Display for AerogpuStageResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnknownShaderStage { shader_stage } => {
                write!(f, "unknown shader_stage value {shader_stage}")
            }
            Self::UnknownStageEx { stage_ex } => write!(f, "unknown stage_ex value {stage_ex}"),
            Self::InvalidStageEx { stage_ex } => write!(
                f,
                "invalid stage_ex value {stage_ex} (Pixel/Vertex must be encoded via shader_stage)"
            ),
        }
    }
}

/// Resolve the effective D3D shader stage, applying the `stage_ex` override rules described in
/// [`AerogpuShaderStageEx`].
///
/// - `shader_stage` is the packet's base `enum aerogpu_shader_stage` value.
/// - `reserved0` is the packet's `reserved0` field (repurposed as `stage_ex` when
///   `shader_stage == Compute`).
pub fn resolve_stage(
    shader_stage: u32,
    reserved0: u32,
) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
    match shader_stage {
        0 => Ok(AerogpuD3dShaderStage::Vertex),
        1 => Ok(AerogpuD3dShaderStage::Pixel),
        3 => Ok(AerogpuD3dShaderStage::Geometry),
        2 => match reserved0 {
            0 => Ok(AerogpuD3dShaderStage::Compute),
            2 => Ok(AerogpuD3dShaderStage::Geometry),
            3 => Ok(AerogpuD3dShaderStage::Hull),
            4 => Ok(AerogpuD3dShaderStage::Domain),
            // Compute program type (5) is accepted as an alias for legacy Compute.
            5 => Ok(AerogpuD3dShaderStage::Compute),
            // Vertex program type (1) must be encoded via shader_stage for clarity.
            1 => Err(AerogpuStageResolveError::InvalidStageEx {
                stage_ex: reserved0,
            }),
            other => Err(AerogpuStageResolveError::UnknownStageEx { stage_ex: other }),
        },
        other => Err(AerogpuStageResolveError::UnknownShaderStage {
            shader_stage: other,
        }),
    }
}

/// Effective shader stage resolved from a legacy `shader_stage` (VS/PS/CS/GS) plus an optional
/// `stage_ex` discriminator in a trailing `reserved0` u32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuShaderStageResolved {
    Vertex,
    Pixel,
    Compute,
    Geometry,
    Hull,
    Domain,
    /// Unknown/unsupported value (either an invalid legacy stage, or an unknown stage_ex).
    Unknown {
        shader_stage: u32,
        stage_ex: u32,
    },
}

/// Resolve the effective stage from `(shader_stage, reserved0)` according to the stage_ex rules.
///
/// Forward-compat:
/// - Unknown legacy stages are preserved as `Unknown { shader_stage, stage_ex: reserved0 }`.
/// - Unknown `stage_ex` values are preserved as `Unknown { shader_stage, stage_ex }`.
pub fn resolve_shader_stage_with_ex(
    shader_stage: u32,
    reserved0: u32,
) -> AerogpuShaderStageResolved {
    match AerogpuShaderStage::from_u32(shader_stage) {
        Some(AerogpuShaderStage::Vertex) => AerogpuShaderStageResolved::Vertex,
        Some(AerogpuShaderStage::Pixel) => AerogpuShaderStageResolved::Pixel,
        Some(AerogpuShaderStage::Geometry) => AerogpuShaderStageResolved::Geometry,
        Some(AerogpuShaderStage::Compute) => {
            if reserved0 == 0 {
                return AerogpuShaderStageResolved::Compute;
            }
            match AerogpuShaderStageEx::from_u32(reserved0) {
                Some(AerogpuShaderStageEx::Geometry) => AerogpuShaderStageResolved::Geometry,
                Some(AerogpuShaderStageEx::Hull) => AerogpuShaderStageResolved::Hull,
                Some(AerogpuShaderStageEx::Domain) => AerogpuShaderStageResolved::Domain,
                Some(AerogpuShaderStageEx::Compute) => AerogpuShaderStageResolved::Compute,
                // `reserved0 == 0` is handled above, but keep this arm for completeness.
                Some(AerogpuShaderStageEx::None) => AerogpuShaderStageResolved::Compute,
                None => AerogpuShaderStageResolved::Unknown {
                    shader_stage,
                    stage_ex: reserved0,
                },
            }
        }
        None => AerogpuShaderStageResolved::Unknown {
            shader_stage,
            stage_ex: reserved0,
        },
    }
}

/// Resolve the effective stage from `(shader_stage, reserved0)`, gated by command stream ABI minor.
///
/// For command streams older than [`AEROGPU_STAGE_EX_MIN_ABI_MINOR`], `reserved0` is ignored when
/// `shader_stage == Compute` to preserve legacy behavior.
pub fn resolve_shader_stage_with_ex_gated(
    abi_minor: u16,
    shader_stage: u32,
    reserved0: u32,
) -> AerogpuShaderStageResolved {
    if abi_minor < AEROGPU_STAGE_EX_MIN_ABI_MINOR
        && shader_stage == AerogpuShaderStage::Compute as u32
    {
        return resolve_shader_stage_with_ex(shader_stage, 0);
    }
    resolve_shader_stage_with_ex(shader_stage, reserved0)
}

/// Encode the `reserved0` value for packets that support `stage_ex`.
///
/// `reserved0` is only interpreted as a `stage_ex` tag when `shader_stage == Compute`.
///
/// Writers should emit `reserved0 = 0` for legacy/no-override Compute packets.
pub fn encode_stage_ex_reserved0(
    shader_stage: AerogpuShaderStage,
    stage_ex: Option<AerogpuShaderStageEx>,
) -> u32 {
    match shader_stage {
        AerogpuShaderStage::Compute => match stage_ex {
            None | Some(AerogpuShaderStageEx::None) => 0,
            // Canonicalize Compute to legacy encoding.
            Some(AerogpuShaderStageEx::Compute) => 0,
            Some(ex) => ex as u32,
        },
        other => match stage_ex {
            None | Some(AerogpuShaderStageEx::None) => 0,
            Some(ex) => panic!(
                "stage_ex ({ex:?}) may only be encoded when shader_stage==COMPUTE (got {other:?})"
            ),
        },
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuIndexFormat {
    Uint16 = 0,
    Uint32 = 1,
}

impl AerogpuIndexFormat {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Uint16),
            1 => Some(Self::Uint32),
            _ => None,
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuSamplerFilter {
    Nearest = 0,
    Linear = 1,
}

impl AerogpuSamplerFilter {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Nearest),
            1 => Some(Self::Linear),
            _ => None,
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuSamplerAddressMode {
    ClampToEdge = 0,
    Repeat = 1,
    MirrorRepeat = 2,
}

impl AerogpuSamplerAddressMode {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::ClampToEdge),
            1 => Some(Self::Repeat),
            2 => Some(Self::MirrorRepeat),
            _ => None,
        }
    }
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

    LineListAdj = 10,
    LineStripAdj = 11,
    TriangleListAdj = 12,
    TriangleStripAdj = 13,

    PatchList1 = 33,
    PatchList2 = 34,
    PatchList3 = 35,
    PatchList4 = 36,
    PatchList5 = 37,
    PatchList6 = 38,
    PatchList7 = 39,
    PatchList8 = 40,
    PatchList9 = 41,
    PatchList10 = 42,
    PatchList11 = 43,
    PatchList12 = 44,
    PatchList13 = 45,
    PatchList14 = 46,
    PatchList15 = 47,
    PatchList16 = 48,
    PatchList17 = 49,
    PatchList18 = 50,
    PatchList19 = 51,
    PatchList20 = 52,
    PatchList21 = 53,
    PatchList22 = 54,
    PatchList23 = 55,
    PatchList24 = 56,
    PatchList25 = 57,
    PatchList26 = 58,
    PatchList27 = 59,
    PatchList28 = 60,
    PatchList29 = 61,
    PatchList30 = 62,
    PatchList31 = 63,
    PatchList32 = 64,
}

impl AerogpuPrimitiveTopology {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::PointList),
            2 => Some(Self::LineList),
            3 => Some(Self::LineStrip),
            4 => Some(Self::TriangleList),
            5 => Some(Self::TriangleStrip),
            6 => Some(Self::TriangleFan),

            10 => Some(Self::LineListAdj),
            11 => Some(Self::LineStripAdj),
            12 => Some(Self::TriangleListAdj),
            13 => Some(Self::TriangleStripAdj),

            33..=64 => {
                const PATCHLISTS: [AerogpuPrimitiveTopology; 32] = [
                    AerogpuPrimitiveTopology::PatchList1,
                    AerogpuPrimitiveTopology::PatchList2,
                    AerogpuPrimitiveTopology::PatchList3,
                    AerogpuPrimitiveTopology::PatchList4,
                    AerogpuPrimitiveTopology::PatchList5,
                    AerogpuPrimitiveTopology::PatchList6,
                    AerogpuPrimitiveTopology::PatchList7,
                    AerogpuPrimitiveTopology::PatchList8,
                    AerogpuPrimitiveTopology::PatchList9,
                    AerogpuPrimitiveTopology::PatchList10,
                    AerogpuPrimitiveTopology::PatchList11,
                    AerogpuPrimitiveTopology::PatchList12,
                    AerogpuPrimitiveTopology::PatchList13,
                    AerogpuPrimitiveTopology::PatchList14,
                    AerogpuPrimitiveTopology::PatchList15,
                    AerogpuPrimitiveTopology::PatchList16,
                    AerogpuPrimitiveTopology::PatchList17,
                    AerogpuPrimitiveTopology::PatchList18,
                    AerogpuPrimitiveTopology::PatchList19,
                    AerogpuPrimitiveTopology::PatchList20,
                    AerogpuPrimitiveTopology::PatchList21,
                    AerogpuPrimitiveTopology::PatchList22,
                    AerogpuPrimitiveTopology::PatchList23,
                    AerogpuPrimitiveTopology::PatchList24,
                    AerogpuPrimitiveTopology::PatchList25,
                    AerogpuPrimitiveTopology::PatchList26,
                    AerogpuPrimitiveTopology::PatchList27,
                    AerogpuPrimitiveTopology::PatchList28,
                    AerogpuPrimitiveTopology::PatchList29,
                    AerogpuPrimitiveTopology::PatchList30,
                    AerogpuPrimitiveTopology::PatchList31,
                    AerogpuPrimitiveTopology::PatchList32,
                ];
                Some(PATCHLISTS[(v - 33) as usize])
            }
            _ => None,
        }
    }
}

pub const AEROGPU_RESOURCE_USAGE_NONE: u32 = 0;
pub const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = 1u32 << 0;
pub const AEROGPU_RESOURCE_USAGE_INDEX_BUFFER: u32 = 1u32 << 1;
pub const AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER: u32 = 1u32 << 2;
pub const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = 1u32 << 3;
pub const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1u32 << 4;
pub const AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL: u32 = 1u32 << 5;
pub const AEROGPU_RESOURCE_USAGE_SCANOUT: u32 = 1u32 << 6;
pub const AEROGPU_RESOURCE_USAGE_STORAGE: u32 = 1u32 << 7;

pub const AEROGPU_COPY_FLAG_NONE: u32 = 0;
pub const AEROGPU_COPY_FLAG_WRITEBACK_DST: u32 = 1u32 << 0;

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

impl AerogpuCmdCreateBuffer {
    pub const SIZE_BYTES: usize = 40;
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

impl AerogpuCmdCreateTexture2d {
    pub const SIZE_BYTES: usize = 56;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCreateTextureView {
    pub hdr: AerogpuCmdHdr,
    pub view_handle: AerogpuHandle,
    pub texture_handle: AerogpuHandle,
    pub format: u32, // aerogpu_format
    pub base_mip_level: u32,
    pub mip_level_count: u32,
    pub base_array_layer: u32,
    pub array_layer_count: u32,
    pub reserved0: u64,
}

impl AerogpuCmdCreateTextureView {
    pub const SIZE_BYTES: usize = 44;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDestroyResource {
    pub hdr: AerogpuCmdHdr,
    pub resource_handle: AerogpuHandle,
    pub reserved0: u32,
}

impl AerogpuCmdDestroyResource {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDestroyTextureView {
    pub hdr: AerogpuCmdHdr,
    pub view_handle: AerogpuHandle,
    pub reserved0: u32,
}

impl AerogpuCmdDestroyTextureView {
    pub const SIZE_BYTES: usize = 16;
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

impl AerogpuCmdResourceDirtyRange {
    pub const SIZE_BYTES: usize = 32;
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

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCopyBuffer {
    pub hdr: AerogpuCmdHdr,
    pub dst_buffer: AerogpuHandle,
    pub src_buffer: AerogpuHandle,
    pub dst_offset_bytes: u64,
    pub src_offset_bytes: u64,
    pub size_bytes: u64,
    pub flags: u32, // aerogpu_copy_flags
    pub reserved0: u32,
}

impl AerogpuCmdCopyBuffer {
    pub const SIZE_BYTES: usize = 48;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCopyTexture2d {
    pub hdr: AerogpuCmdHdr,
    pub dst_texture: AerogpuHandle,
    pub src_texture: AerogpuHandle,
    pub dst_mip_level: u32,
    pub dst_array_layer: u32,
    pub src_mip_level: u32,
    pub src_array_layer: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub src_x: u32,
    pub src_y: u32,
    pub width: u32,
    pub height: u32,
    pub flags: u32, // aerogpu_copy_flags
    pub reserved0: u32,
}

impl AerogpuCmdCopyTexture2d {
    pub const SIZE_BYTES: usize = 64;
}
/* -------------------------------- Shaders -------------------------------- */

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCreateShaderDxbc {
    pub hdr: AerogpuCmdHdr,
    pub shader_handle: AerogpuHandle,
    /// Shader stage selector (legacy enum).
    ///
    /// stage_ex extension:
    /// - If `stage == AerogpuShaderStage::Compute` and `reserved0 != 0`, then `reserved0` is treated
    ///   as `AerogpuShaderStageEx` (DXBC program type numbering), allowing the guest to describe a
    ///   GS/HS/DS shader without changing the base struct layout.
    /// - `reserved0 == 0` means legacy compute (no override).
    pub stage: u32,
    pub dxbc_size_bytes: u32,
    /// `stage_ex` ABI extension tag.
    ///
    /// Used by `CREATE_SHADER_DXBC` to represent additional DXBC program types (GS/HS/DS) without
    /// extending the legacy `stage` enum.
    ///
    /// Encoding:
    /// - Legacy: `stage = VERTEX/PIXEL/GEOMETRY/COMPUTE` and `stage_ex = 0`.
    /// - Stage-ex: set `stage = COMPUTE` and set `stage_ex` to a non-zero DXBC program type:
    ///   - GS: `stage_ex = GEOMETRY` (2) (alternative to legacy `stage = GEOMETRY` where supported)
    ///   - HS: `stage_ex = HULL`     (3)
    ///   - DS: `stage_ex = DOMAIN`   (4)
    ///
    /// Note: `stage_ex == 0` is reserved for legacy/default (old guests always write 0 into
    /// reserved fields). As a result, DXBC `stage_ex == 0` (Pixel) is not encodable here; pixel
    /// shaders must use the legacy `stage = PIXEL` encoding.
    pub reserved0: u32,
}

impl AerogpuCmdCreateShaderDxbc {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.stage, self.reserved0)
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDestroyShader {
    pub hdr: AerogpuCmdHdr,
    pub shader_handle: AerogpuHandle,
    pub reserved0: u32,
}

impl AerogpuCmdDestroyShader {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdBindShaders {
    pub hdr: AerogpuCmdHdr,
    pub vs: AerogpuHandle,
    pub ps: AerogpuHandle,
    pub cs: AerogpuHandle,
    /// Base packet size is 24 bytes (hdr + vs/ps/cs/reserved0).
    ///
    /// Legacy GS extension (24-byte packet):
    /// - If `hdr.size_bytes == 24` and `reserved0 != 0`, `reserved0` is interpreted as the geometry
    ///   shader handle (`gs`).
    ///
    /// Append-only extension (>= 36-byte packet):
    /// - If trailing handles are present, they are decoded into [`BindShadersEx`] (`{gs, hs, ds}`)
    ///   and must take precedence.
    /// - In the extended form, this field should be 0 unless the emitter chooses to mirror `gs` here
    ///   for best-effort compatibility; if mirrored, it should match the appended `gs` handle. The
    ///   appended `{gs, hs, ds}` handles are authoritative.
    /// - Any additional trailing bytes beyond the known fields must be ignored for forward-compat.
    pub reserved0: u32,
}

impl AerogpuCmdBindShaders {
    pub const SIZE_BYTES: usize = 24;
    /// Extended BIND_SHADERS packet size (base struct + appended `{gs,hs,ds}` handles).
    pub const EX_SIZE_BYTES: usize = Self::SIZE_BYTES + 3 * core::mem::size_of::<AerogpuHandle>();
    /// Extended BIND_SHADERS payload size (excluding the 8-byte command header).
    pub const EX_PAYLOAD_SIZE_BYTES: usize =
        (Self::SIZE_BYTES - AerogpuCmdHdr::SIZE_BYTES) + 3 * core::mem::size_of::<AerogpuHandle>();

    /// Geometry shader handle (GS).
    ///
    /// ABI note:
    /// - Legacy encoding: `aerogpu_cmd_bind_shaders.reserved0` is treated as the GS handle when
    ///   non-zero.
    /// - Extended encoding (`hdr.size_bytes >= 36`): `{gs, hs, ds}` are appended after the base
    ///   struct, and those appended fields are authoritative (this helper only exposes the legacy
    ///   `reserved0` field; if used as a compatibility mirror, it should match the appended `gs`).
    pub const fn gs(&self) -> AerogpuHandle {
        self.reserved0
    }

    /// Set the geometry shader handle (GS).
    ///
    /// ABI note: this sets the legacy `reserved0` field. For the extended packet form, emitters
    /// should also write the appended `{gs, hs, ds}` handles.
    pub fn set_gs(&mut self, gs: AerogpuHandle) {
        self.reserved0 = gs;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindShadersEx {
    /// Geometry shader handle.
    pub gs: AerogpuHandle,
    /// Hull shader handle.
    pub hs: AerogpuHandle,
    /// Domain shader handle.
    pub ds: AerogpuHandle,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetShaderConstantsF {
    pub hdr: AerogpuCmdHdr,
    /// Shader stage selector (legacy enum).
    ///
    /// stage_ex extension:
    /// - If `stage == AerogpuShaderStage::Compute` and `reserved0 != 0`, `reserved0` is treated as
    ///   `AerogpuShaderStageEx` (DXBC program type numbering). Values 2/3/4 correspond to GS/HS/DS.
    /// - `reserved0 == 0` means legacy compute (no override).
    pub stage: u32,
    pub start_register: u32,
    pub vec4_count: u32,
    /// `stage_ex` when `stage == AEROGPU_SHADER_STAGE_COMPUTE`.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetShaderConstantsF {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.stage, self.reserved0)
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetShaderConstantsI {
    pub hdr: AerogpuCmdHdr,
    /// Shader stage selector (legacy enum).
    pub stage: u32,
    pub start_register: u32,
    pub vec4_count: u32,
    /// `stage_ex` when `stage == AEROGPU_SHADER_STAGE_COMPUTE`.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetShaderConstantsI {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.stage, self.reserved0)
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetShaderConstantsB {
    pub hdr: AerogpuCmdHdr,
    /// Shader stage selector (legacy enum).
    pub stage: u32,
    pub start_register: u32,
    pub bool_count: u32,
    /// `stage_ex` when `stage == AEROGPU_SHADER_STAGE_COMPUTE`.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetShaderConstantsB {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.stage, self.reserved0)
    }
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
    /// FNV-1a 32-bit hash of the semantic name after canonicalizing to ASCII uppercase.
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
    Constant = 6,
    InvConstant = 7,
}

impl AerogpuBlendFactor {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Zero),
            1 => Some(Self::One),
            2 => Some(Self::SrcAlpha),
            3 => Some(Self::InvSrcAlpha),
            4 => Some(Self::DestAlpha),
            5 => Some(Self::InvDestAlpha),
            6 => Some(Self::Constant),
            7 => Some(Self::InvConstant),
            _ => None,
        }
    }
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

impl AerogpuBlendOp {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Add),
            1 => Some(Self::Subtract),
            2 => Some(Self::RevSubtract),
            3 => Some(Self::Min),
            4 => Some(Self::Max),
            _ => None,
        }
    }
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
    pub src_factor_alpha: u32,
    pub dst_factor_alpha: u32,
    pub blend_op_alpha: u32,
    pub blend_constant_rgba_f32: [u32; 4],
    pub sample_mask: u32,
}

impl AerogpuBlendState {
    pub const SIZE_BYTES: usize = 52;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetBlendState {
    pub hdr: AerogpuCmdHdr,
    pub state: AerogpuBlendState,
}

impl AerogpuCmdSetBlendState {
    pub const SIZE_BYTES: usize = 60;
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

impl AerogpuCompareFunc {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Never),
            1 => Some(Self::Less),
            2 => Some(Self::Equal),
            3 => Some(Self::LessEqual),
            4 => Some(Self::Greater),
            5 => Some(Self::NotEqual),
            6 => Some(Self::GreaterEqual),
            7 => Some(Self::Always),
            _ => None,
        }
    }
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

impl AerogpuDepthStencilState {
    pub const SIZE_BYTES: usize = 20;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetDepthStencilState {
    pub hdr: AerogpuCmdHdr,
    pub state: AerogpuDepthStencilState,
}

impl AerogpuCmdSetDepthStencilState {
    pub const SIZE_BYTES: usize = 28;
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuFillMode {
    Solid = 0,
    Wireframe = 1,
}

impl AerogpuFillMode {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Solid),
            1 => Some(Self::Wireframe),
            _ => None,
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCullMode {
    None = 0,
    Front = 1,
    Back = 2,
}

impl AerogpuCullMode {
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Front),
            2 => Some(Self::Back),
            _ => None,
        }
    }
}

// AerogpuRasterizerState.flags bits.
//
// Default value 0 corresponds to D3D11 defaults:
// - DepthClipEnable = TRUE
pub const AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE: u32 = 1u32 << 0;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuRasterizerState {
    pub fill_mode: u32,
    pub cull_mode: u32,
    pub front_ccw: u32,
    pub scissor_enable: u32,
    pub depth_bias: i32,
    pub flags: u32,
}

impl AerogpuRasterizerState {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetRasterizerState {
    pub hdr: AerogpuCmdHdr,
    pub state: AerogpuRasterizerState,
}

impl AerogpuCmdSetRasterizerState {
    pub const SIZE_BYTES: usize = 32;
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

impl AerogpuCmdSetRenderTargets {
    pub const SIZE_BYTES: usize = 48;
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

impl AerogpuCmdSetViewport {
    pub const SIZE_BYTES: usize = 32;
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

impl AerogpuCmdSetScissor {
    pub const SIZE_BYTES: usize = 24;
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

impl AerogpuCmdSetVertexBuffers {
    pub const SIZE_BYTES: usize = 16;
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

impl AerogpuCmdSetIndexBuffer {
    pub const SIZE_BYTES: usize = 24;
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
    /// Legacy shader stage (`AerogpuShaderStage`).
    pub shader_stage: u32,
    pub slot: u32,
    pub texture: AerogpuHandle,
    /// `stage_ex` when `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE`.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetTexture {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_shader_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.shader_stage, self.reserved0)
    }
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

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdCreateSampler {
    pub hdr: AerogpuCmdHdr,
    pub sampler_handle: AerogpuHandle,
    pub filter: u32,
    pub address_u: u32,
    pub address_v: u32,
    pub address_w: u32,
}

impl AerogpuCmdCreateSampler {
    pub const SIZE_BYTES: usize = 28;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDestroySampler {
    pub hdr: AerogpuCmdHdr,
    pub sampler_handle: AerogpuHandle,
    pub reserved0: u32,
}

impl AerogpuCmdDestroySampler {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetSamplers {
    pub hdr: AerogpuCmdHdr,
    /// Legacy shader stage (`AerogpuShaderStage`).
    pub shader_stage: u32,
    pub start_slot: u32,
    pub sampler_count: u32,
    /// `stage_ex` when `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE`.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetSamplers {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_shader_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.shader_stage, self.reserved0)
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuConstantBufferBinding {
    pub buffer: AerogpuHandle,
    pub offset_bytes: u32,
    pub size_bytes: u32,
    pub reserved0: u32,
}

impl AerogpuConstantBufferBinding {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetConstantBuffers {
    pub hdr: AerogpuCmdHdr,
    /// Legacy shader stage (`AerogpuShaderStage`).
    pub shader_stage: u32,
    pub start_slot: u32,
    pub buffer_count: u32,
    /// `stage_ex` when `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE`.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetConstantBuffers {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_shader_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.shader_stage, self.reserved0)
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuShaderResourceBufferBinding {
    pub buffer: AerogpuHandle,
    pub offset_bytes: u32,
    pub size_bytes: u32,
    pub reserved0: u32,
}

impl AerogpuShaderResourceBufferBinding {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetShaderResourceBuffers {
    pub hdr: AerogpuCmdHdr,
    /// Legacy shader stage (`AerogpuShaderStage`).
    pub shader_stage: u32,
    pub start_slot: u32,
    pub buffer_count: u32,
    /// `stage_ex` ABI extension tag.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetShaderResourceBuffers {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_shader_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.shader_stage, self.reserved0)
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuUnorderedAccessBufferBinding {
    pub buffer: AerogpuHandle,
    pub offset_bytes: u32,
    pub size_bytes: u32,
    pub initial_count: u32,
}

impl AerogpuUnorderedAccessBufferBinding {
    pub const SIZE_BYTES: usize = 16;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdSetUnorderedAccessBuffers {
    pub hdr: AerogpuCmdHdr,
    /// Legacy shader stage (`AerogpuShaderStage`).
    pub shader_stage: u32,
    pub start_slot: u32,
    pub uav_count: u32,
    /// `stage_ex` ABI extension tag.
    ///
    /// See [`AerogpuShaderStageEx`] for encoding rules.
    pub reserved0: u32,
}

impl AerogpuCmdSetUnorderedAccessBuffers {
    pub const SIZE_BYTES: usize = 24;

    pub fn resolved_shader_stage(&self) -> Result<AerogpuD3dShaderStage, AerogpuStageResolveError> {
        resolve_stage(self.shader_stage, self.reserved0)
    }
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

impl AerogpuCmdClear {
    pub const SIZE_BYTES: usize = 36;
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

impl AerogpuCmdDraw {
    pub const SIZE_BYTES: usize = 24;
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

impl AerogpuCmdDrawIndexed {
    pub const SIZE_BYTES: usize = 28;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdDispatch {
    pub hdr: AerogpuCmdHdr,
    pub group_count_x: u32,
    pub group_count_y: u32,
    pub group_count_z: u32,
    pub reserved0: u32,
}

impl AerogpuCmdDispatch {
    pub const SIZE_BYTES: usize = 24;
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

impl AerogpuCmdPresent {
    pub const SIZE_BYTES: usize = 16;
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

impl AerogpuCmdPresentEx {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdExportSharedSurface {
    pub hdr: AerogpuCmdHdr,
    pub resource_handle: AerogpuHandle,
    pub reserved0: u32,
    pub share_token: u64,
}

impl AerogpuCmdExportSharedSurface {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdImportSharedSurface {
    pub hdr: AerogpuCmdHdr,
    pub out_resource_handle: AerogpuHandle,
    pub reserved0: u32,
    pub share_token: u64,
}

impl AerogpuCmdImportSharedSurface {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdReleaseSharedSurface {
    pub hdr: AerogpuCmdHdr,
    pub share_token: u64,
    pub reserved0: u64,
}

impl AerogpuCmdReleaseSharedSurface {
    pub const SIZE_BYTES: usize = 24;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuCmdFlush {
    pub hdr: AerogpuCmdHdr,
    pub reserved0: u32,
    pub reserved1: u32,
}

impl AerogpuCmdFlush {
    pub const SIZE_BYTES: usize = 16;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuCmdDecodeError {
    BufferTooSmall,
    BadMagic {
        found: u32,
    },
    Abi(AerogpuAbiError),
    BadSizeBytes {
        found: u32,
    },
    SizeNotAligned {
        found: u32,
    },
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

impl fmt::Display for AerogpuCmdDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AerogpuCmdDecodeError::BufferTooSmall => write!(f, "buffer too small"),
            AerogpuCmdDecodeError::BadMagic { found } => write!(f, "bad magic 0x{found:08X}"),
            AerogpuCmdDecodeError::Abi(err) => write!(f, "abi error: {err}"),
            AerogpuCmdDecodeError::BadSizeBytes { found } => write!(f, "bad size_bytes {found}"),
            AerogpuCmdDecodeError::SizeNotAligned { found } => {
                write!(f, "size_bytes {found} is not 4-byte aligned")
            }
            AerogpuCmdDecodeError::PacketOverrunsStream {
                offset,
                packet_size_bytes,
                stream_size_bytes,
            } => write!(
                f,
                "packet overruns stream: offset={offset} packet_size_bytes={packet_size_bytes} stream_size_bytes={stream_size_bytes}"
            ),
            AerogpuCmdDecodeError::UnexpectedOpcode { found, expected } => write!(
                f,
                "unexpected opcode 0x{found:08X} (expected {expected:?})"
            ),
            AerogpuCmdDecodeError::PayloadSizeMismatch { expected, found } => write!(
                f,
                "payload size mismatch (expected {expected} bytes, found {found} bytes)"
            ),
            AerogpuCmdDecodeError::CountOverflow => write!(f, "count overflow"),
        }
    }
}

impl std::error::Error for AerogpuCmdDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AerogpuCmdDecodeError::Abi(err) => Some(err),
            _ => None,
        }
    }
}

pub fn decode_cmd_stream_header_le(
    buf: &[u8],
) -> Result<AerogpuCmdStreamHeader, AerogpuCmdDecodeError> {
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

pub fn validate_cmd_stream_header(
    hdr: &AerogpuCmdStreamHeader,
) -> Result<(), AerogpuCmdDecodeError> {
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
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_create_shader_dxbc_payload_le()
}

/// Decode BIND_SHADERS and return optional appended `{gs, hs, ds}` handles.
pub fn decode_cmd_bind_shaders_payload_le(
    buf: &[u8],
) -> Result<(AerogpuCmdBindShaders, Option<BindShadersEx>), AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_bind_shaders_payload_le()
}

/// Decode CREATE_INPUT_LAYOUT and return the blob payload (without padding).
pub fn decode_cmd_create_input_layout_blob_le(
    buf: &[u8],
) -> Result<(AerogpuCmdCreateInputLayout, &[u8]), AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_create_input_layout_payload_le()
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
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_set_shader_constants_f_payload_le()
}

/// Decode SET_SHADER_CONSTANTS_I and return the int payload.
pub fn decode_cmd_set_shader_constants_i_payload_le(
    buf: &[u8],
) -> Result<(AerogpuCmdSetShaderConstantsI, Vec<i32>), AerogpuCmdDecodeError> {
    if buf.len() < AerogpuCmdSetShaderConstantsI::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let hdr = decode_cmd_hdr_le(buf)?;
    if hdr.opcode != AerogpuCmdOpcode::SetShaderConstantsI as u32 {
        return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
            found: hdr.opcode,
            expected: AerogpuCmdOpcode::SetShaderConstantsI,
        });
    }
    let packet_len = validate_packet_len(buf, hdr)?;

    let vec4_count = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    let i32_count = vec4_count
        .checked_mul(4)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)? as usize;
    let payload_size_bytes = i32_count
        .checked_mul(4)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    let payload_start = AerogpuCmdSetShaderConstantsI::SIZE_BYTES;
    let payload_end = payload_start
        .checked_add(payload_size_bytes)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    if payload_end > packet_len {
        return Err(AerogpuCmdDecodeError::BadSizeBytes {
            found: hdr.size_bytes,
        });
    }

    let cmd = AerogpuCmdSetShaderConstantsI {
        hdr,
        stage: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        start_register: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        vec4_count,
        reserved0: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
    };

    let mut out = Vec::new();
    out.try_reserve_exact(i32_count)
        .map_err(|_| AerogpuCmdDecodeError::CountOverflow)?;
    for i in 0..i32_count {
        let off = payload_start + i * 4;
        out.push(i32::from_le_bytes(buf[off..off + 4].try_into().unwrap()));
    }

    Ok((cmd, out))
}

/// Decode SET_SHADER_CONSTANTS_B and return the bool payload as raw u32 values.
///
/// Payload encoding: `uint32_t data[bool_count]` where each element is 0 or 1.
pub fn decode_cmd_set_shader_constants_b_payload_le(
    buf: &[u8],
) -> Result<(AerogpuCmdSetShaderConstantsB, Vec<u32>), AerogpuCmdDecodeError> {
    if buf.len() < AerogpuCmdSetShaderConstantsB::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let hdr = decode_cmd_hdr_le(buf)?;
    if hdr.opcode != AerogpuCmdOpcode::SetShaderConstantsB as u32 {
        return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
            found: hdr.opcode,
            expected: AerogpuCmdOpcode::SetShaderConstantsB,
        });
    }
    let packet_len = validate_packet_len(buf, hdr)?;

    let bool_count = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    let payload_size_bytes = (bool_count as usize)
        .checked_mul(4)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    let payload_start = AerogpuCmdSetShaderConstantsB::SIZE_BYTES;
    let payload_end = payload_start
        .checked_add(payload_size_bytes)
        .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
    if payload_end > packet_len {
        return Err(AerogpuCmdDecodeError::BadSizeBytes {
            found: hdr.size_bytes,
        });
    }

    let cmd = AerogpuCmdSetShaderConstantsB {
        hdr,
        stage: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        start_register: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        bool_count,
        reserved0: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
    };

    let mut out = Vec::new();
    out.try_reserve_exact(bool_count as usize)
        .map_err(|_| AerogpuCmdDecodeError::CountOverflow)?;
    for i in 0..bool_count as usize {
        let off = payload_start + i * 4;
        out.push(u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()));
    }

    Ok((cmd, out))
}

/// Decode UPLOAD_RESOURCE and return the raw payload bytes (without padding).
pub fn decode_cmd_upload_resource_payload_le(
    buf: &[u8],
) -> Result<(AerogpuCmdUploadResource, &[u8]), AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_upload_resource_payload_le()
}

/// Decode COPY_BUFFER.
pub fn decode_cmd_copy_buffer_le(
    buf: &[u8],
) -> Result<AerogpuCmdCopyBuffer, AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_copy_buffer_payload_le()
}

/// Decode COPY_TEXTURE2D.
pub fn decode_cmd_copy_texture2d_le(
    buf: &[u8],
) -> Result<AerogpuCmdCopyTexture2d, AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_copy_texture2d_payload_le()
}

/// Decode DISPATCH.
pub fn decode_cmd_dispatch_le(buf: &[u8]) -> Result<AerogpuCmdDispatch, AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_dispatch_payload_le()
}

/// Decode SET_VERTEX_BUFFERS and parse the trailing `aerogpu_vertex_buffer_binding[]`.
pub fn decode_cmd_set_vertex_buffers_bindings_le(
    buf: &[u8],
) -> Result<(AerogpuCmdSetVertexBuffers, &[AerogpuVertexBufferBinding]), AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_set_vertex_buffers_payload_le()
}

/// Decode SET_TEXTURE.
pub fn decode_cmd_set_texture_le(
    buf: &[u8],
) -> Result<AerogpuCmdSetTexture, AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_set_texture_payload_le()
}

/// Decode SET_SAMPLERS and parse the trailing `aerogpu_handle_t samplers[]`.
pub fn decode_cmd_set_samplers_handles_le(
    buf: &[u8],
) -> Result<(AerogpuCmdSetSamplers, &[AerogpuHandle]), AerogpuCmdDecodeError> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_set_samplers_payload_le()
}

/// Decode SET_CONSTANT_BUFFERS and parse the trailing `aerogpu_constant_buffer_binding[]`.
pub fn decode_cmd_set_constant_buffers_bindings_le(
    buf: &[u8],
) -> Result<
    (
        AerogpuCmdSetConstantBuffers,
        &[AerogpuConstantBufferBinding],
    ),
    AerogpuCmdDecodeError,
> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_set_constant_buffers_payload_le()
}

/// Decode SET_SHADER_RESOURCE_BUFFERS and parse the trailing `aerogpu_shader_resource_buffer_binding[]`.
pub fn decode_cmd_set_shader_resource_buffers_bindings_le(
    buf: &[u8],
) -> Result<
    (
        AerogpuCmdSetShaderResourceBuffers,
        &[AerogpuShaderResourceBufferBinding],
    ),
    AerogpuCmdDecodeError,
> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_set_shader_resource_buffers_payload_le()
}

/// Decode SET_UNORDERED_ACCESS_BUFFERS and parse the trailing `aerogpu_unordered_access_buffer_binding[]`.
pub fn decode_cmd_set_unordered_access_buffers_bindings_le(
    buf: &[u8],
) -> Result<
    (
        AerogpuCmdSetUnorderedAccessBuffers,
        &[AerogpuUnorderedAccessBufferBinding],
    ),
    AerogpuCmdDecodeError,
> {
    let hdr = decode_cmd_hdr_le(buf)?;
    let packet_len = validate_packet_len(buf, hdr)?;
    let packet = AerogpuCmdPacket {
        hdr,
        opcode: AerogpuCmdOpcode::from_u32(hdr.opcode),
        payload: &buf[AerogpuCmdHdr::SIZE_BYTES..packet_len],
    };
    packet.decode_set_unordered_access_buffers_payload_le()
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

/// Returns whether the command stream contains a vsync'd PRESENT.
///
/// This is used by AeroGPU device models to implement the Win7 timing contract:
/// a vsync'd present fence must not complete before the *next* vblank edge.
///
/// The submit-level hint bit (`AEROGPU_SUBMIT_FLAG_PRESENT`) is not sufficient on its own; device
/// models must inspect the command stream contents (PRESENT/PRESENT_EX packets with the VSYNC
/// flag set).
pub fn cmd_stream_has_vsync_present_bytes(bytes: &[u8]) -> Result<bool, AerogpuCmdDecodeError> {
    let iter = AerogpuCmdStreamIter::new(bytes)?;
    for packet in iter {
        let packet = packet?;
        if matches!(
            packet.opcode,
            Some(AerogpuCmdOpcode::Present) | Some(AerogpuCmdOpcode::PresentEx)
        ) {
            // flags is always after the scanout_id field.
            if packet.payload.len() < 8 {
                return Err(AerogpuCmdDecodeError::PayloadSizeMismatch {
                    expected: 8,
                    found: packet.payload.len(),
                });
            }
            let flags = u32::from_le_bytes(packet.payload[4..8].try_into().unwrap());
            if (flags & AEROGPU_PRESENT_FLAG_VSYNC) != 0 {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Returns whether the command stream contains a vsync'd PRESENT without requiring the caller to
/// build a full byte slice of the stream.
///
/// `read` must copy `buf.len()` bytes starting at `gpa` into `buf`.
///
/// This helper is intended for device models that want to inspect command stream contents for fence
/// pacing, but do not want to allocate/copy potentially large streams in the common case.
pub fn cmd_stream_has_vsync_present_reader<F>(
    mut read: F,
    cmd_gpa: u64,
    cmd_size_bytes: u32,
) -> Result<bool, AerogpuCmdDecodeError>
where
    F: FnMut(u64, &mut [u8]),
{
    let cmd_size =
        usize::try_from(cmd_size_bytes).map_err(|_| AerogpuCmdDecodeError::CountOverflow)?;
    if cmd_size < AerogpuCmdStreamHeader::SIZE_BYTES {
        return Err(AerogpuCmdDecodeError::BufferTooSmall);
    }

    let mut stream_hdr_bytes = [0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
    read(cmd_gpa, &mut stream_hdr_bytes);
    let stream_hdr = decode_cmd_stream_header_le(&stream_hdr_bytes)?;
    if stream_hdr.size_bytes > cmd_size_bytes {
        return Err(AerogpuCmdDecodeError::PacketOverrunsStream {
            offset: 0,
            packet_size_bytes: stream_hdr.size_bytes,
            stream_size_bytes: cmd_size_bytes,
        });
    }
    let declared_size = stream_hdr.size_bytes as usize;

    let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    while offset < declared_size {
        let rem = declared_size - offset;
        if rem < AerogpuCmdHdr::SIZE_BYTES {
            return Err(AerogpuCmdDecodeError::PacketOverrunsStream {
                offset: u32::try_from(offset).map_err(|_| AerogpuCmdDecodeError::CountOverflow)?,
                packet_size_bytes: AerogpuCmdHdr::SIZE_BYTES as u32,
                stream_size_bytes: stream_hdr.size_bytes,
            });
        }

        let cmd_hdr_gpa = cmd_gpa
            .checked_add(offset as u64)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        let mut cmd_hdr_bytes = [0u8; AerogpuCmdHdr::SIZE_BYTES];
        read(cmd_hdr_gpa, &mut cmd_hdr_bytes);
        let cmd_hdr = decode_cmd_hdr_le(&cmd_hdr_bytes)?;

        let packet_size = cmd_hdr.size_bytes as usize;
        let end = offset
            .checked_add(packet_size)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        if end > declared_size {
            return Err(AerogpuCmdDecodeError::PacketOverrunsStream {
                offset: u32::try_from(offset).map_err(|_| AerogpuCmdDecodeError::CountOverflow)?,
                packet_size_bytes: cmd_hdr.size_bytes,
                stream_size_bytes: stream_hdr.size_bytes,
            });
        }

        if cmd_hdr.opcode == AerogpuCmdOpcode::Present as u32
            || cmd_hdr.opcode == AerogpuCmdOpcode::PresentEx as u32
        {
            // flags is always at offset 12 (hdr + scanout_id).
            let payload_len = packet_size.saturating_sub(AerogpuCmdHdr::SIZE_BYTES);
            if payload_len < 8 {
                return Err(AerogpuCmdDecodeError::PayloadSizeMismatch {
                    expected: 8,
                    found: payload_len,
                });
            }
            let flags_gpa = cmd_hdr_gpa
                .checked_add(12)
                .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
            let mut flags_bytes = [0u8; 4];
            read(flags_gpa, &mut flags_bytes);
            let flags = u32::from_le_bytes(flags_bytes);
            if (flags & AEROGPU_PRESENT_FLAG_VSYNC) != 0 {
                return Ok(true);
            }
        }

        offset = end;
    }

    Ok(false)
}

pub struct AerogpuCmdStreamView<'a> {
    pub header: AerogpuCmdStreamHeader,
    pub packets: Vec<AerogpuCmdPacket<'a>>,
}

impl<'a> AerogpuCmdStreamView<'a> {
    pub fn decode_from_le_bytes(buf: &'a [u8]) -> Result<Self, AerogpuCmdDecodeError> {
        let iter = AerogpuCmdStreamIter::new(buf)?;
        let header = *iter.header();
        let mut packets = Vec::new();
        for pkt in iter {
            let pkt = pkt?;
            packets
                .try_reserve(1)
                .map_err(|_| AerogpuCmdDecodeError::CountOverflow)?;
            packets.push(pkt);
        }
        Ok(Self { header, packets })
    }
}

fn align_up_4(size: usize) -> Result<usize, AerogpuCmdDecodeError> {
    size.checked_add(3)
        .map(|v| v & !3usize)
        .ok_or(AerogpuCmdDecodeError::CountOverflow)
}

fn validate_expected_payload_size(
    expected: usize,
    payload: &[u8],
) -> Result<(), AerogpuCmdDecodeError> {
    // Forward-compat: allow packets to grow by appending new fields after the existing payload.
    //
    // The command stream format includes a per-packet `size_bytes` specifically so newer guest
    // drivers can extend packets without breaking older hosts. For variable-sized packets this
    // means `payload.len()` can be larger than what the current decoder understands; we only
    // require the prefix we need to be present.
    if payload.len() < expected {
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

    pub fn decode_bind_shaders_payload_le(
        &self,
    ) -> Result<(AerogpuCmdBindShaders, Option<BindShadersEx>), AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::BindShaders) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::BindShaders,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let vs = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let ps = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let cs = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        let ex = if self.payload.len() >= AerogpuCmdBindShaders::EX_PAYLOAD_SIZE_BYTES {
            // Extended BIND_SHADERS appends `{gs, hs, ds}`.
            Some(BindShadersEx {
                gs: u32::from_le_bytes(self.payload[16..20].try_into().unwrap()),
                hs: u32::from_le_bytes(self.payload[20..24].try_into().unwrap()),
                ds: u32::from_le_bytes(self.payload[24..28].try_into().unwrap()),
            })
        } else {
            None
        };

        Ok((
            AerogpuCmdBindShaders {
                hdr: self.hdr,
                vs,
                ps,
                cs,
                reserved0,
            },
            ex,
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
            usize::try_from(size_bytes).map_err(|_| AerogpuCmdDecodeError::BadSizeBytes {
                found: self.hdr.size_bytes,
            })?;
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

    pub fn decode_copy_buffer_payload_le(
        &self,
    ) -> Result<AerogpuCmdCopyBuffer, AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::CopyBuffer) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::CopyBuffer,
            });
        }
        let expected_payload_size = size_of::<AerogpuCmdCopyBuffer>() - AerogpuCmdHdr::SIZE_BYTES;
        validate_expected_payload_size(expected_payload_size, self.payload)?;
        Ok(AerogpuCmdCopyBuffer {
            hdr: self.hdr,
            dst_buffer: u32::from_le_bytes(self.payload[0..4].try_into().unwrap()),
            src_buffer: u32::from_le_bytes(self.payload[4..8].try_into().unwrap()),
            dst_offset_bytes: u64::from_le_bytes(self.payload[8..16].try_into().unwrap()),
            src_offset_bytes: u64::from_le_bytes(self.payload[16..24].try_into().unwrap()),
            size_bytes: u64::from_le_bytes(self.payload[24..32].try_into().unwrap()),
            flags: u32::from_le_bytes(self.payload[32..36].try_into().unwrap()),
            reserved0: u32::from_le_bytes(self.payload[36..40].try_into().unwrap()),
        })
    }

    pub fn decode_copy_texture2d_payload_le(
        &self,
    ) -> Result<AerogpuCmdCopyTexture2d, AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::CopyTexture2d) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::CopyTexture2d,
            });
        }
        let expected_payload_size =
            size_of::<AerogpuCmdCopyTexture2d>() - AerogpuCmdHdr::SIZE_BYTES;
        validate_expected_payload_size(expected_payload_size, self.payload)?;
        Ok(AerogpuCmdCopyTexture2d {
            hdr: self.hdr,
            dst_texture: u32::from_le_bytes(self.payload[0..4].try_into().unwrap()),
            src_texture: u32::from_le_bytes(self.payload[4..8].try_into().unwrap()),
            dst_mip_level: u32::from_le_bytes(self.payload[8..12].try_into().unwrap()),
            dst_array_layer: u32::from_le_bytes(self.payload[12..16].try_into().unwrap()),
            src_mip_level: u32::from_le_bytes(self.payload[16..20].try_into().unwrap()),
            src_array_layer: u32::from_le_bytes(self.payload[20..24].try_into().unwrap()),
            dst_x: u32::from_le_bytes(self.payload[24..28].try_into().unwrap()),
            dst_y: u32::from_le_bytes(self.payload[28..32].try_into().unwrap()),
            src_x: u32::from_le_bytes(self.payload[32..36].try_into().unwrap()),
            src_y: u32::from_le_bytes(self.payload[36..40].try_into().unwrap()),
            width: u32::from_le_bytes(self.payload[40..44].try_into().unwrap()),
            height: u32::from_le_bytes(self.payload[44..48].try_into().unwrap()),
            flags: u32::from_le_bytes(self.payload[48..52].try_into().unwrap()),
            reserved0: u32::from_le_bytes(self.payload[52..56].try_into().unwrap()),
        })
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

    pub fn decode_set_shader_constants_f_payload_le(
        &self,
    ) -> Result<(AerogpuCmdSetShaderConstantsF, Vec<f32>), AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::SetShaderConstantsF) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::SetShaderConstantsF,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let stage = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let start_register = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let vec4_count = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        let float_count = vec4_count
            .checked_mul(4)
            .ok_or(AerogpuCmdDecodeError::BufferTooSmall)? as usize;
        let payload_size_bytes = float_count
            .checked_mul(4)
            .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
        let payload_start = 16usize;
        let payload_end = payload_start
            .checked_add(payload_size_bytes)
            .ok_or(AerogpuCmdDecodeError::BufferTooSmall)?;
        if payload_end > self.payload.len() {
            return Err(AerogpuCmdDecodeError::BadSizeBytes {
                found: self.hdr.size_bytes,
            });
        }

        let cmd = AerogpuCmdSetShaderConstantsF {
            hdr: self.hdr,
            stage,
            start_register,
            vec4_count,
            reserved0,
        };

        let mut out = Vec::new();
        out.try_reserve_exact(float_count)
            .map_err(|_| AerogpuCmdDecodeError::CountOverflow)?;
        for i in 0..float_count {
            let off = payload_start + i * 4;
            let bits = u32::from_le_bytes(self.payload[off..off + 4].try_into().unwrap());
            out.push(f32::from_bits(bits));
        }

        Ok((cmd, out))
    }

    pub fn decode_set_vertex_buffers_payload_le(
        &self,
    ) -> Result<(AerogpuCmdSetVertexBuffers, &'a [AerogpuVertexBufferBinding]), AerogpuCmdDecodeError>
    {
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

        let binding_bytes = &self.payload[8..8 + binding_bytes_len];
        let (prefix, bindings, suffix) =
            unsafe { binding_bytes.align_to::<AerogpuVertexBufferBinding>() };
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

    pub fn decode_set_texture_payload_le(
        &self,
    ) -> Result<AerogpuCmdSetTexture, AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::SetTexture) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::SetTexture,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let shader_stage = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let slot = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let texture = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        validate_expected_payload_size(16, self.payload)?;
        Ok(AerogpuCmdSetTexture {
            hdr: self.hdr,
            shader_stage,
            slot,
            texture,
            reserved0,
        })
    }

    pub fn decode_dispatch_payload_le(&self) -> Result<AerogpuCmdDispatch, AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::Dispatch) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::Dispatch,
            });
        }
        let expected_payload_size = size_of::<AerogpuCmdDispatch>() - AerogpuCmdHdr::SIZE_BYTES;
        validate_expected_payload_size(expected_payload_size, self.payload)?;
        Ok(AerogpuCmdDispatch {
            hdr: self.hdr,
            group_count_x: u32::from_le_bytes(self.payload[0..4].try_into().unwrap()),
            group_count_y: u32::from_le_bytes(self.payload[4..8].try_into().unwrap()),
            group_count_z: u32::from_le_bytes(self.payload[8..12].try_into().unwrap()),
            reserved0: u32::from_le_bytes(self.payload[12..16].try_into().unwrap()),
        })
    }

    pub fn decode_set_samplers_payload_le(
        &self,
    ) -> Result<(AerogpuCmdSetSamplers, &'a [AerogpuHandle]), AerogpuCmdDecodeError> {
        if self.opcode != Some(AerogpuCmdOpcode::SetSamplers) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::SetSamplers,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let shader_stage = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let start_slot = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let sampler_count = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        let sampler_count_usize = sampler_count as usize;
        let handles_bytes_len = sampler_count_usize
            .checked_mul(core::mem::size_of::<AerogpuHandle>())
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        let handles_end = 16usize
            .checked_add(handles_bytes_len)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        if handles_end > self.payload.len() {
            return Err(AerogpuCmdDecodeError::BadSizeBytes {
                found: self.hdr.size_bytes,
            });
        }

        let handles_bytes = &self.payload[16..handles_end];
        let (prefix, handles, suffix) = unsafe { handles_bytes.align_to::<AerogpuHandle>() };
        if !prefix.is_empty() || !suffix.is_empty() || handles.len() != sampler_count_usize {
            return Err(AerogpuCmdDecodeError::CountOverflow);
        }

        Ok((
            AerogpuCmdSetSamplers {
                hdr: self.hdr,
                shader_stage,
                start_slot,
                sampler_count,
                reserved0,
            },
            handles,
        ))
    }

    pub fn decode_set_constant_buffers_payload_le(
        &self,
    ) -> Result<
        (
            AerogpuCmdSetConstantBuffers,
            &'a [AerogpuConstantBufferBinding],
        ),
        AerogpuCmdDecodeError,
    > {
        if self.opcode != Some(AerogpuCmdOpcode::SetConstantBuffers) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::SetConstantBuffers,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let shader_stage = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let start_slot = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let buffer_count = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        let buffer_count_usize = buffer_count as usize;
        let binding_bytes_len = buffer_count_usize
            .checked_mul(core::mem::size_of::<AerogpuConstantBufferBinding>())
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        let binding_end = 16usize
            .checked_add(binding_bytes_len)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        if binding_end > self.payload.len() {
            return Err(AerogpuCmdDecodeError::BadSizeBytes {
                found: self.hdr.size_bytes,
            });
        }

        let binding_bytes = &self.payload[16..binding_end];
        let (prefix, bindings, suffix) =
            unsafe { binding_bytes.align_to::<AerogpuConstantBufferBinding>() };
        if !prefix.is_empty() || !suffix.is_empty() || bindings.len() != buffer_count_usize {
            return Err(AerogpuCmdDecodeError::CountOverflow);
        }

        Ok((
            AerogpuCmdSetConstantBuffers {
                hdr: self.hdr,
                shader_stage,
                start_slot,
                buffer_count,
                reserved0,
            },
            bindings,
        ))
    }

    pub fn decode_set_shader_resource_buffers_payload_le(
        &self,
    ) -> Result<
        (
            AerogpuCmdSetShaderResourceBuffers,
            &'a [AerogpuShaderResourceBufferBinding],
        ),
        AerogpuCmdDecodeError,
    > {
        if self.opcode != Some(AerogpuCmdOpcode::SetShaderResourceBuffers) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::SetShaderResourceBuffers,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let shader_stage = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let start_slot = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let buffer_count = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        let buffer_count_usize = buffer_count as usize;
        let binding_bytes_len = buffer_count_usize
            .checked_mul(core::mem::size_of::<AerogpuShaderResourceBufferBinding>())
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        let binding_end = 16usize
            .checked_add(binding_bytes_len)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        if binding_end > self.payload.len() {
            return Err(AerogpuCmdDecodeError::BadSizeBytes {
                found: self.hdr.size_bytes,
            });
        }

        let binding_bytes = &self.payload[16..binding_end];
        let (prefix, bindings, suffix) =
            unsafe { binding_bytes.align_to::<AerogpuShaderResourceBufferBinding>() };
        if !prefix.is_empty() || !suffix.is_empty() || bindings.len() != buffer_count_usize {
            return Err(AerogpuCmdDecodeError::CountOverflow);
        }

        Ok((
            AerogpuCmdSetShaderResourceBuffers {
                hdr: self.hdr,
                shader_stage,
                start_slot,
                buffer_count,
                reserved0,
            },
            bindings,
        ))
    }

    pub fn decode_set_unordered_access_buffers_payload_le(
        &self,
    ) -> Result<
        (
            AerogpuCmdSetUnorderedAccessBuffers,
            &'a [AerogpuUnorderedAccessBufferBinding],
        ),
        AerogpuCmdDecodeError,
    > {
        if self.opcode != Some(AerogpuCmdOpcode::SetUnorderedAccessBuffers) {
            return Err(AerogpuCmdDecodeError::UnexpectedOpcode {
                found: self.hdr.opcode,
                expected: AerogpuCmdOpcode::SetUnorderedAccessBuffers,
            });
        }
        if self.payload.len() < 16 {
            return Err(AerogpuCmdDecodeError::BufferTooSmall);
        }

        let shader_stage = u32::from_le_bytes(self.payload[0..4].try_into().unwrap());
        let start_slot = u32::from_le_bytes(self.payload[4..8].try_into().unwrap());
        let uav_count = u32::from_le_bytes(self.payload[8..12].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(self.payload[12..16].try_into().unwrap());

        let uav_count_usize = uav_count as usize;
        let binding_bytes_len = uav_count_usize
            .checked_mul(core::mem::size_of::<AerogpuUnorderedAccessBufferBinding>())
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        let binding_end = 16usize
            .checked_add(binding_bytes_len)
            .ok_or(AerogpuCmdDecodeError::CountOverflow)?;
        if binding_end > self.payload.len() {
            return Err(AerogpuCmdDecodeError::BadSizeBytes {
                found: self.hdr.size_bytes,
            });
        }

        let binding_bytes = &self.payload[16..binding_end];
        let (prefix, bindings, suffix) =
            unsafe { binding_bytes.align_to::<AerogpuUnorderedAccessBufferBinding>() };
        if !prefix.is_empty() || !suffix.is_empty() || bindings.len() != uav_count_usize {
            return Err(AerogpuCmdDecodeError::CountOverflow);
        }

        Ok((
            AerogpuCmdSetUnorderedAccessBuffers {
                hdr: self.hdr,
                shader_stage,
                start_slot,
                uav_count,
                reserved0,
            },
            bindings,
        ))
    }
}
