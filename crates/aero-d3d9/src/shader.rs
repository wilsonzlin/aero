//! Shader parsing and translation (DXBC/D3D9 bytecode → IR → WGSL).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use blake3::Hash;
use thiserror::Error;

use crate::dxbc;
use crate::shader_limits::{
    MAX_D3D9_ATTR_OUTPUT_REGISTER_INDEX, MAX_D3D9_COLOR_OUTPUT_REGISTER_INDEX,
    MAX_D3D9_INPUT_REGISTER_INDEX, MAX_D3D9_SAMPLER_REGISTER_INDEX, MAX_D3D9_SHADER_BLOB_BYTES,
    MAX_D3D9_SHADER_BYTECODE_BYTES, MAX_D3D9_SHADER_CONTROL_FLOW_NESTING,
    MAX_D3D9_SHADER_REGISTER_INDEX, MAX_D3D9_SHADER_TOKEN_COUNT, MAX_D3D9_TEMP_REGISTER_INDEX,
    MAX_D3D9_TEXCOORD_OUTPUT_REGISTER_INDEX, MAX_D3D9_TEXTURE_REGISTER_INDEX,
};
use crate::sm3::decode::TextureType;
use crate::vertex::{AdaptiveLocationMap, DeclUsage, LocationMapError, VertexLocationMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Pixel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderModel {
    pub major: u8,
    pub minor: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderVersion {
    pub stage: ShaderStage,
    pub model: ShaderModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegisterFile {
    Temp,        // r#
    Input,       // v#
    Const,       // c#
    ConstBool,   // b#
    Addr,        // a#
    Texture,     // t#
    Sampler,     // s#
    RastOut,     // oPos
    AttrOut,     // oD#
    TexCoordOut, // oT#
    ColorOut,    // oC#
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Register {
    pub file: RegisterFile,
    pub index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Swizzle(pub [u8; 4]);

impl Swizzle {
    pub const XYZW: Swizzle = Swizzle([0, 1, 2, 3]);

    pub fn from_d3d_byte(swz: u8) -> Self {
        // 2 bits per component, x in bits 0..1, y in 2..3, z in 4..5, w in 6..7.
        let comp = |shift: u32| (swz >> shift) & 0b11u8;
        Self([comp(0), comp(2), comp(4), comp(6)])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WriteMask(pub u8);

impl WriteMask {
    pub const XYZW: WriteMask = WriteMask(0b1111);

    pub fn write_x(self) -> bool {
        self.0 & 0b0001 != 0
    }
    pub fn write_y(self) -> bool {
        self.0 & 0b0010 != 0
    }
    pub fn write_z(self) -> bool {
        self.0 & 0b0100 != 0
    }
    pub fn write_w(self) -> bool {
        self.0 & 0b1000 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SrcModifier {
    None,
    Negate,
    Bias,
    BiasNegate,
    Sign,
    SignNegate,
    Comp,
    X2,
    X2Negate,
    Dz,
    Dw,
    Abs,
    AbsNegate,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Src {
    pub reg: Register,
    pub swizzle: Swizzle,
    pub modifier: SrcModifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Dst {
    pub reg: Register,
    pub mask: WriteMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    Nop,
    Mov,
    Add,
    Sub,
    Mul,
    Mad,
    Lrp,
    Dp2,
    /// `dp2add`: dot2 + add (`dot(a.xy, b.xy) + c.x`), replicated to all components.
    Dp2Add,
    Dp3,
    Dp4,
    Exp,
    Log,
    Pow,
    Rcp,
    Rsq,
    Min,
    Max,
    Cmp,
    Slt,
    Sge,
    Seq,
    Sne,
    /// `dsx`: screen-space x derivative (ddx), replicated per component.
    Dsx,
    /// `dsy`: screen-space y derivative (ddy), replicated per component.
    Dsy,
    Frc,
    If,
    Ifc,
    Else,
    EndIf,
    Texld,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultShift {
    None,
    Mul2,
    Mul4,
    Mul8,
    Div2,
    Div4,
    Div8,
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResultModifier {
    pub saturate: bool,
    pub shift: ResultShift,
}

impl Default for ResultModifier {
    fn default() -> Self {
        Self {
            saturate: false,
            shift: ResultShift::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub op: Op,
    pub dst: Option<Dst>,
    pub src: Vec<Src>,
    /// Sampler register for `texld` (s#).
    pub sampler: Option<u16>,
    /// Extra immediate payload for opcodes that need it (e.g. `ifc` compare op code).
    pub imm: Option<u32>,
    pub result_modifier: ResultModifier,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderProgram {
    pub version: ShaderVersion,
    pub instructions: Vec<Instruction>,
    /// `def c#` constants embedded in the shader bytecode.
    pub const_defs_f32: BTreeMap<u16, [f32; 4]>,
    /// `defb b#` constants embedded in the shader bytecode.
    pub const_defs_bool: BTreeMap<u16, bool>,
    pub used_samplers: BTreeSet<u16>,
    /// Texture type declared for each sampler register (`dcl_2d`, `dcl_cube`, etc).
    ///
    /// The AeroGPU D3D9 executor supports `texture_2d<f32>` and `texture_cube<f32>` samplers.
    /// Used samplers declared as other texture types (3D/1D/unknown) are rejected by the
    /// high-level shader translator (`shader_translate`); unused declarations are tolerated.
    pub sampler_texture_types: HashMap<u16, TextureType>,
    pub used_consts: BTreeSet<u16>,
    pub used_inputs: BTreeSet<u16>,
    pub used_outputs: BTreeSet<Register>,
    pub temp_count: u16,
    /// True when vertex shader input registers were remapped from raw `v#` indices to canonical
    /// WGSL `@location(n)` values based on `dcl_*` semantics.
    ///
    /// When this is true, the host-side D3D9 executor must bind vertex attributes using the same
    /// semantic-based mapping.
    pub uses_semantic_locations: bool,

    /// Semantic → WGSL location mapping derived from vertex shader `dcl_*` declarations when
    /// [`ShaderProgram::uses_semantic_locations`] is true.
    ///
    /// This is required by host-side executors to bind vertex buffers consistently with the
    /// semantic-based remapping performed during shader translation.
    pub semantic_locations: Vec<SemanticLocation>,
}

/// Semantic-to-location mapping entry produced by semantic-based vertex input remapping.
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticLocation {
    pub usage: DeclUsage,
    pub usage_index: u8,
    pub location: u32,
}

#[derive(Debug, Error)]
pub enum ShaderError {
    #[error("dxbc error: {0}")]
    Dxbc(#[from] dxbc::DxbcError),
    #[error("shader bytecode length {len} exceeds maximum {max} bytes")]
    BytecodeTooLarge { len: usize, max: usize },
    #[error("shader token count {count} exceeds maximum {max} tokens")]
    TokenCountTooLarge { count: usize, max: usize },
    #[error("shader token stream too small")]
    TokenStreamTooSmall,
    #[error("unsupported shader version token 0x{0:08x}")]
    UnsupportedVersion(u32),
    #[error("unexpected end of instruction stream")]
    UnexpectedEof,
    #[error("unknown or unsupported opcode 0x{0:04x}")]
    UnsupportedOpcode(u16),
    #[error("unsupported register type {0}")]
    UnsupportedRegisterType(u8),
    #[error("unsupported source modifier {0}")]
    UnsupportedSrcModifier(u8),
    #[error("unsupported ifc comparison op {0}")]
    UnsupportedCompareOp(u8),
    #[error("register index {index} in {file:?} exceeds maximum {max}")]
    RegisterIndexTooLarge {
        file: RegisterFile,
        index: u16,
        max: u32,
    },
    #[error("invalid control flow: {0}")]
    InvalidControlFlow(&'static str),
    #[error(transparent)]
    LocationMap(#[from] LocationMapError),
    #[error(
        "vertex shader input DCL declarations map multiple input registers to WGSL @location({location}): v{first} and v{second}"
    )]
    DuplicateInputLocation {
        location: u32,
        first: u16,
        second: u16,
    },
    #[error("unsupported sampler texture type {ty:?} for s{sampler}")]
    UnsupportedSamplerTextureType { sampler: u32, ty: TextureType },
    #[error("invalid destination register file {file:?} in {stage:?} shader")]
    InvalidDstRegisterFile {
        stage: ShaderStage,
        file: RegisterFile,
    },
    #[error("invalid source register file {file:?} in {stage:?} shader")]
    InvalidSrcRegisterFile {
        stage: ShaderStage,
        file: RegisterFile,
    },
}

fn read_u32(words: &[u32], idx: &mut usize) -> Result<u32, ShaderError> {
    if *idx >= words.len() {
        return Err(ShaderError::UnexpectedEof);
    }
    let v = words[*idx];
    *idx += 1;
    Ok(v)
}

fn decode_reg_type(token: u32) -> u8 {
    // D3D9 encodes register type in 5 bits split across two fields:
    // - 3 bits at 28..30
    // - 2 bits at 11..12 (shifted down by 8 to become bits 3..4)
    let low = ((token >> 28) & 0x7) as u8;
    let high = ((token >> 8) & 0x18) as u8;
    low | high
}

fn decode_reg_num(token: u32) -> u16 {
    (token & 0x7FF) as u16
}

fn decode_src_modifier(modifier: u8) -> Result<SrcModifier, ShaderError> {
    // Matches `D3DSHADER_PARAM_SRCMOD_TYPE` / `D3DSHADER_SRCMOD` encoding.
    Ok(match modifier {
        0 => SrcModifier::None,
        1 => SrcModifier::Negate,
        2 => SrcModifier::Bias,
        3 => SrcModifier::BiasNegate,
        4 => SrcModifier::Sign,
        5 => SrcModifier::SignNegate,
        6 => SrcModifier::Comp,
        7 => SrcModifier::X2,
        8 => SrcModifier::X2Negate,
        9 => SrcModifier::Dz,
        10 => SrcModifier::Dw,
        11 => SrcModifier::Abs,
        12 => SrcModifier::AbsNegate,
        13 => SrcModifier::Not,
        other => return Err(ShaderError::UnsupportedSrcModifier(other)),
    })
}

fn decode_src(token: u32) -> Result<Src, ShaderError> {
    let reg_type = decode_reg_type(token);
    let reg_num = decode_reg_num(token);
    let swizzle_byte = ((token >> 16) & 0xFF) as u8;
    let modifier_raw = ((token >> 24) & 0xF) as u8;
    let modifier = decode_src_modifier(modifier_raw)?;

    let file = match reg_type {
        0 => RegisterFile::Temp,
        1 => RegisterFile::Input,
        2 => RegisterFile::Const,
        14 => RegisterFile::ConstBool,
        3 => RegisterFile::Texture, // also Addr in vs; we treat as texture for pixel shader inputs.
        10 => RegisterFile::Sampler,
        other => return Err(ShaderError::UnsupportedRegisterType(other)),
    };

    let max = match file {
        RegisterFile::Temp => MAX_D3D9_TEMP_REGISTER_INDEX,
        RegisterFile::Input => MAX_D3D9_INPUT_REGISTER_INDEX,
        RegisterFile::Const => MAX_D3D9_SHADER_REGISTER_INDEX,
        RegisterFile::ConstBool => MAX_D3D9_SHADER_REGISTER_INDEX,
        RegisterFile::Texture => MAX_D3D9_TEXTURE_REGISTER_INDEX,
        RegisterFile::Sampler => MAX_D3D9_SAMPLER_REGISTER_INDEX,
        // Source operands should never use output register files, but keep a defensive fallback.
        _ => MAX_D3D9_SHADER_REGISTER_INDEX,
    };
    if u32::from(reg_num) > max {
        return Err(ShaderError::RegisterIndexTooLarge {
            file,
            index: reg_num,
            max,
        });
    }

    Ok(Src {
        reg: Register {
            file,
            index: reg_num,
        },
        swizzle: Swizzle::from_d3d_byte(swizzle_byte),
        modifier,
    })
}

fn validate_dst_for_stage(stage: ShaderStage, dst: &Dst) -> Result<(), ShaderError> {
    let ok = match stage {
        ShaderStage::Vertex => matches!(
            dst.reg.file,
            RegisterFile::Temp
                | RegisterFile::RastOut
                | RegisterFile::AttrOut
                | RegisterFile::TexCoordOut
        ),
        ShaderStage::Pixel => matches!(dst.reg.file, RegisterFile::Temp | RegisterFile::ColorOut),
    };
    if ok {
        Ok(())
    } else {
        Err(ShaderError::InvalidDstRegisterFile {
            stage,
            file: dst.reg.file,
        })
    }
}

fn validate_src_for_stage(stage: ShaderStage, src: &Src) -> Result<(), ShaderError> {
    // Sampler registers are not general-purpose numeric sources; they are only valid as the
    // dedicated `texld` sampler operand.
    if src.reg.file == RegisterFile::Sampler {
        return Err(ShaderError::InvalidSrcRegisterFile {
            stage,
            file: src.reg.file,
        });
    }
    // The legacy translator does not model the vertex-shader address register file (`a#`).
    // D3D9 encodes it using the same raw register-type as pixel shader `t#` inputs, so reject it
    // in vertex shaders to avoid generating invalid WGSL.
    if stage == ShaderStage::Vertex && src.reg.file == RegisterFile::Texture {
        return Err(ShaderError::InvalidSrcRegisterFile {
            stage,
            file: src.reg.file,
        });
    }
    Ok(())
}

fn decode_dst(token: u32) -> Result<Dst, ShaderError> {
    let reg_type = decode_reg_type(token);
    let reg_num = decode_reg_num(token);
    let mask = ((token >> 16) & 0xF) as u8;

    let file = match reg_type {
        0 => RegisterFile::Temp,
        1 => RegisterFile::Input,
        4 => RegisterFile::RastOut,
        5 => RegisterFile::AttrOut,
        6 => RegisterFile::TexCoordOut,
        8 => RegisterFile::ColorOut,
        other => return Err(ShaderError::UnsupportedRegisterType(other)),
    };

    let max = match file {
        RegisterFile::Temp => MAX_D3D9_TEMP_REGISTER_INDEX,
        RegisterFile::Input => MAX_D3D9_INPUT_REGISTER_INDEX,
        RegisterFile::Const => MAX_D3D9_SHADER_REGISTER_INDEX,
        RegisterFile::ConstBool => MAX_D3D9_SHADER_REGISTER_INDEX,
        RegisterFile::Texture => MAX_D3D9_TEXTURE_REGISTER_INDEX,
        RegisterFile::Sampler => MAX_D3D9_SAMPLER_REGISTER_INDEX,
        RegisterFile::AttrOut => MAX_D3D9_ATTR_OUTPUT_REGISTER_INDEX,
        RegisterFile::TexCoordOut => MAX_D3D9_TEXCOORD_OUTPUT_REGISTER_INDEX,
        RegisterFile::ColorOut => MAX_D3D9_COLOR_OUTPUT_REGISTER_INDEX,
        // `oPos` is special and doesn't use the numeric index in our codegen. Allow the full
        // conservative cap to avoid rejecting shaders that write other raster outputs.
        RegisterFile::RastOut | RegisterFile::Addr => MAX_D3D9_SHADER_REGISTER_INDEX,
    };
    if u32::from(reg_num) > max {
        return Err(ShaderError::RegisterIndexTooLarge {
            file,
            index: reg_num,
            max,
        });
    }

    Ok(Dst {
        reg: Register {
            file,
            index: reg_num,
        },
        mask: WriteMask(mask),
    })
}

fn decode_result_modifier(opcode_token: u32) -> ResultModifier {
    let mod_bits = ((opcode_token >> 20) & 0xF) as u8;
    let saturate = (mod_bits & 0x1) != 0;
    let shift_bits = (mod_bits >> 1) & 0x7;
    let shift = match shift_bits {
        0 => ResultShift::None,
        1 => ResultShift::Mul2,
        2 => ResultShift::Mul4,
        3 => ResultShift::Mul8,
        4 => ResultShift::Div2,
        5 => ResultShift::Div4,
        6 => ResultShift::Div8,
        other => ResultShift::Unknown(other),
    };
    ResultModifier { saturate, shift }
}

fn opcode_to_op(opcode: u16) -> Option<Op> {
    // Based on D3DSHADER_INSTRUCTION_OPCODE_TYPE values.
    match opcode {
        0x0000 => Some(Op::Nop),
        0x0001 => Some(Op::Mov),
        0x0002 => Some(Op::Add),
        0x0003 => Some(Op::Sub),
        0x0004 => Some(Op::Mad),
        0x0005 => Some(Op::Mul),
        0x0006 => Some(Op::Rcp),
        0x0007 => Some(Op::Rsq),
        0x0008 => Some(Op::Dp3),
        0x0009 => Some(Op::Dp4),
        0x000A => Some(Op::Min),
        0x000B => Some(Op::Max),
        0x000C => Some(Op::Slt),
        0x000D => Some(Op::Sge),
        0x000E => Some(Op::Exp),
        0x000F => Some(Op::Log),
        0x0012 => Some(Op::Lrp),
        0x0013 => Some(Op::Frc),
        0x0020 => Some(Op::Pow),
        0x0028 => Some(Op::If),
        0x0029 => Some(Op::Ifc),
        0x002A => Some(Op::Else),
        0x002B => Some(Op::EndIf),
        0x0042 => Some(Op::Texld), // D3DSIO_TEX
        0x0054 => Some(Op::Seq),
        0x0055 => Some(Op::Sne),
        0x0056 => Some(Op::Dsx),
        0x0057 => Some(Op::Dsy),
        0x0058 => Some(Op::Cmp),
        0x0059 => Some(Op::Dp2Add),
        0x005A => Some(Op::Dp2),
        0xFFFF => Some(Op::End),
        _ => None,
    }
}

fn parse_token_stream(token_bytes: &[u8]) -> Result<ShaderProgram, ShaderError> {
    if token_bytes.len() < 4 {
        return Err(ShaderError::TokenStreamTooSmall);
    }
    if !token_bytes.len().is_multiple_of(4) {
        return Err(ShaderError::TokenStreamTooSmall);
    }
    if token_bytes.len() > MAX_D3D9_SHADER_BYTECODE_BYTES {
        return Err(ShaderError::BytecodeTooLarge {
            len: token_bytes.len(),
            max: MAX_D3D9_SHADER_BYTECODE_BYTES,
        });
    }
    let token_count = token_bytes.len() / 4;
    if token_count > MAX_D3D9_SHADER_TOKEN_COUNT {
        return Err(ShaderError::TokenCountTooLarge {
            count: token_count,
            max: MAX_D3D9_SHADER_TOKEN_COUNT,
        });
    }
    let words: Vec<u32> = token_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    let version_token = words[0];
    let shader_type = (version_token >> 16) as u16;
    let major = ((version_token >> 8) & 0xFF) as u8;
    let minor = (version_token & 0xFF) as u8;
    let stage = match shader_type {
        0xFFFE => ShaderStage::Vertex,
        0xFFFF => ShaderStage::Pixel,
        _ => return Err(ShaderError::UnsupportedVersion(version_token)),
    };
    let supported_version = match major {
        2 => minor <= 1,
        3 => minor == 0,
        _ => false,
    };
    if !supported_version {
        return Err(ShaderError::UnsupportedVersion(version_token));
    }
    let version = ShaderVersion {
        stage,
        model: ShaderModel { major, minor },
    };

    let mut idx = 1usize;
    let mut instructions = Vec::new();
    let mut const_defs_f32 = BTreeMap::<u16, [f32; 4]>::new();
    let mut const_defs_bool = BTreeMap::<u16, bool>::new();
    let mut used_samplers = BTreeSet::new();
    let mut used_consts = BTreeSet::new();
    let mut used_inputs = BTreeSet::new();
    let mut used_outputs = BTreeSet::new();
    let mut temp_max = 0u16;
    let mut if_stack = Vec::<bool>::new(); // tracks whether an `else` has been seen for each active `if`
    let mut input_dcl_map = HashMap::<u16, (DeclUsage, u8)>::new();
    let mut input_dcl_order = Vec::<(DeclUsage, u8)>::new();
    let mut sampler_texture_types = HashMap::<u16, TextureType>::new();
    let mut saw_end = false;

    while idx < words.len() {
        let token = read_u32(&words, &mut idx)?;
        let opcode = (token & 0xFFFF) as u16;
        // D3D9 SM2/SM3 encode instruction length as the *total* number of DWORD tokens in the
        // instruction, including the opcode token itself, in bits 24..27.
        //
        // Higher bits in the same byte are flags (predication/co-issue) and must not affect the
        // decoded length.
        let mut inst_len = ((token >> 24) & 0x0F) as usize;
        let result_modifier = decode_result_modifier(token);

        // Comments are variable-length data blocks that should be skipped.
        // Layout: opcode=0xFFFE, length in DWORDs in bits 16..30.
        if opcode == 0xFFFE {
            let comment_len = ((token >> 16) & 0x7FFF) as usize;
            if idx + comment_len > words.len() {
                return Err(ShaderError::UnexpectedEof);
            }
            idx += comment_len;
            continue;
        }

        if opcode == 0xFFFF {
            // `end` has no operands and does not use the length field.
            inst_len = 1;
        } else if inst_len == 0 {
            // Some opcodes encode length=0 for 1-DWORD instructions (e.g. else/endif).
            inst_len = 1;
        }

        let operand_count = inst_len.saturating_sub(1);
        if idx + operand_count > words.len() {
            return Err(ShaderError::UnexpectedEof);
        }

        let mut params = Vec::with_capacity(operand_count);
        for _ in 0..operand_count {
            params.push(read_u32(&words, &mut idx)?);
        }
        // Predicated SM2/SM3 instructions append a predicate register token at the end of the
        // operand stream. The legacy translator does not model predication; strip the predicate
        // token so fixed-arity opcode decoding sees the expected operand count.
        if (token & 0x1000_0000) != 0 {
            if params.is_empty() {
                return Err(ShaderError::UnexpectedEof);
            }
            params.pop();
        }

        // Declarations (DCL).
        //
        // Layout (SM2/SM3):
        // - `dcl_*` for inputs: opcode token encodes usage/usage_index, followed by a single
        //   destination register token.
        // - Some encodings use an additional "declaration token" operand; accept both so we can
        //   parse fixtures and synthetic shaders.
        //
        // We currently only use vertex shader input declarations to remap D3D9 input registers
        // (`v#`) to canonical WGSL `@location(n)` values.
        if opcode == 0x001F {
            if params.is_empty() {
                return Err(ShaderError::UnexpectedEof);
            }
            // D3D9 `DCL` encoding differs between toolchains:
            // - Modern form: a single destination register token, with usage/texture-type info
            //   packed into the opcode token itself.
            // - Legacy form: a second "decl token" operand (usage in low bits, sampler texture
            //   type in high bits).
            //
            // Be tolerant and accept both.
            let (decl_token, dst_token) = match params.as_slice() {
                // Modern form: `dcl <dst>`
                [dst_token] => (token, *dst_token),
                // Legacy form: `dcl <decl_token>, <dst>`
                [decl_token, dst_token, ..] => (*decl_token, *dst_token),
                _ => return Err(ShaderError::UnexpectedEof),
            };

            // Sampler declaration: `dcl_* s#` (e.g. `dcl_2d s0`, `dcl_cube s1`).
            //
            // The D3D9 token stream encodes sampler texture type in decl_token[27..31] (4 bits).
            // Values match `D3DSAMPLER_TEXTURE_TYPE`:
            //   1 = 1d, 2 = 2d, 3 = cube, 4 = volume (3d)
            //
            // Reject unsupported texture types early with a deterministic error instead of
            // producing WGSL that will fail `wgpu` validation later.
            let dst_reg_type = decode_reg_type(dst_token);
            if dst_reg_type == 10 {
                let sampler = decode_reg_num(dst_token);
                // Modern form: texture type is encoded in opcode_token[16..20].
                // Legacy form: texture type is encoded in decl_token[27..31].
                let tex_ty_raw = if decl_token == token {
                    ((token >> 16) & 0xF) as u8
                } else {
                    ((decl_token >> 27) & 0xF) as u8
                };
                let ty = match tex_ty_raw {
                    1 => TextureType::Texture1D,
                    2 => TextureType::Texture2D,
                    3 => TextureType::TextureCube,
                    4 => TextureType::Texture3D,
                    other => TextureType::Unknown(other),
                };
                sampler_texture_types.insert(sampler, ty);
                if !matches!(
                    ty,
                    TextureType::Texture1D
                        | TextureType::Texture2D
                        | TextureType::TextureCube
                        | TextureType::Texture3D
                ) {
                    return Err(ShaderError::UnsupportedSamplerTextureType {
                        sampler: sampler as u32,
                        ty,
                    });
                }
                continue;
            }

            // D3D9 `DCL` usage encoding:
            // - Modern form (no decl token): usage_raw = opcode_token[16..20], usage_index = opcode_token[20..24].
            // - Legacy form: usage_raw = decl_token[0..5], usage_index = decl_token[16..20].
            let (usage_raw, usage_index) = if decl_token == token {
                (((token >> 16) & 0xF) as u8, ((token >> 20) & 0xF) as u8)
            } else {
                ((decl_token & 0x1F) as u8, ((decl_token >> 16) & 0xF) as u8)
            };
            let Ok(usage) = DeclUsage::from_u8(usage_raw) else {
                continue;
            };
            let Ok(dst) = decode_dst(dst_token) else {
                continue;
            };
            if dst.reg.file == RegisterFile::Input {
                input_dcl_map.insert(dst.reg.index, (usage, usage_index));
                if !input_dcl_order.contains(&(usage, usage_index)) {
                    input_dcl_order.push((usage, usage_index));
                }
            }
            continue;
        }

        // `def` (define float constant) is not part of the executable instruction stream; instead
        // it defines an embedded constant register value (`c#`) that should override the external
        // constant buffer.
        if opcode == 0x0051 {
            if params.len() != 5 {
                return Err(ShaderError::UnexpectedEof);
            }
            let dst_token = params[0];
            let reg_type = decode_reg_type(dst_token);
            if reg_type != 2 {
                return Err(ShaderError::UnsupportedRegisterType(reg_type));
            }
            let reg_num = decode_reg_num(dst_token);
            if u32::from(reg_num) > MAX_D3D9_SHADER_REGISTER_INDEX {
                return Err(ShaderError::RegisterIndexTooLarge {
                    file: RegisterFile::Const,
                    index: reg_num,
                    max: MAX_D3D9_SHADER_REGISTER_INDEX,
                });
            }
            let mut vals = [0f32; 4];
            for i in 0..4 {
                vals[i] = f32::from_bits(params[1 + i]);
            }
            const_defs_f32.insert(reg_num, vals);
            continue;
        }
        // `defb` (define boolean constant) defines an embedded boolean constant register value
        // (`b#`).
        //
        // Boolean registers are scalar in D3D9; we model them as a splatted vec4<f32> with values
        // {0.0, 1.0} so the rest of the legacy translator can continue to treat operands as
        // vectors.
        if opcode == 0x0053 {
            if params.len() != 2 {
                return Err(ShaderError::UnexpectedEof);
            }
            let dst_token = params[0];
            let reg_type = decode_reg_type(dst_token);
            if reg_type != 14 {
                return Err(ShaderError::UnsupportedRegisterType(reg_type));
            }
            let reg_num = decode_reg_num(dst_token);
            if u32::from(reg_num) > MAX_D3D9_SHADER_REGISTER_INDEX {
                return Err(ShaderError::RegisterIndexTooLarge {
                    file: RegisterFile::ConstBool,
                    index: reg_num,
                    max: MAX_D3D9_SHADER_REGISTER_INDEX,
                });
            }
            const_defs_bool.insert(reg_num, params[1] != 0);
            continue;
        }

        // The WGSL backend only implements a subset of SM2/SM3. Treat unknown opcodes as no-ops
        // so we can still translate simple shaders while incrementally adding support.
        let Some(op) = opcode_to_op(opcode) else {
            continue;
        };
        if op == Op::End {
            saw_end = true;
            instructions.push(Instruction {
                op,
                dst: None,
                src: Vec::new(),
                sampler: None,
                imm: None,
                result_modifier,
            });
            break;
        }

        let inst = match op {
            Op::Nop => Instruction {
                op,
                dst: None,
                src: Vec::new(),
                sampler: None,
                imm: None,
                result_modifier,
            },
            Op::If => {
                if params.len() != 1 {
                    return Err(ShaderError::UnexpectedEof);
                }
                if if_stack.len() >= MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
                    return Err(ShaderError::InvalidControlFlow(
                        "control flow nesting exceeds maximum",
                    ));
                }
                if_stack.push(false);
                let cond = decode_src(params[0])?;
                validate_src_for_stage(stage, &cond)?;
                Instruction {
                    op,
                    dst: None,
                    src: vec![cond],
                    sampler: None,
                    imm: None,
                    result_modifier,
                }
            }
            Op::Ifc => {
                if params.len() != 2 {
                    return Err(ShaderError::UnexpectedEof);
                }
                let cmp_code = ((token >> 16) & 0x7) as u8;
                if cmp_code > 5 {
                    return Err(ShaderError::UnsupportedCompareOp(cmp_code));
                }
                if if_stack.len() >= MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
                    return Err(ShaderError::InvalidControlFlow(
                        "control flow nesting exceeds maximum",
                    ));
                }
                if_stack.push(false);
                let src0 = decode_src(params[0])?;
                let src1 = decode_src(params[1])?;
                validate_src_for_stage(stage, &src0)?;
                validate_src_for_stage(stage, &src1)?;
                Instruction {
                    op,
                    dst: None,
                    src: vec![src0, src1],
                    sampler: None,
                    imm: Some(u32::from(cmp_code)),
                    result_modifier,
                }
            }
            Op::Else => {
                if !params.is_empty() {
                    return Err(ShaderError::UnexpectedEof);
                }
                match if_stack.last_mut() {
                    Some(seen_else) => {
                        if *seen_else {
                            return Err(ShaderError::InvalidControlFlow(
                                "multiple else in if block",
                            ));
                        }
                        *seen_else = true;
                    }
                    None => {
                        return Err(ShaderError::InvalidControlFlow("else without matching if"))
                    }
                }
                Instruction {
                    op,
                    dst: None,
                    src: Vec::new(),
                    sampler: None,
                    imm: None,
                    result_modifier,
                }
            }
            Op::EndIf => {
                if !params.is_empty() {
                    return Err(ShaderError::UnexpectedEof);
                }
                if if_stack.pop().is_none() {
                    return Err(ShaderError::InvalidControlFlow("endif without matching if"));
                }
                Instruction {
                    op,
                    dst: None,
                    src: Vec::new(),
                    sampler: None,
                    imm: None,
                    result_modifier,
                }
            }
            Op::Mov
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Mad
            | Op::Lrp
            | Op::Dp2
            | Op::Dp2Add
            | Op::Dp3
            | Op::Dp4
            | Op::Exp
            | Op::Log
            | Op::Rcp
            | Op::Rsq
            | Op::Min
            | Op::Max
            | Op::Cmp
            | Op::Slt
            | Op::Sge
            | Op::Seq
            | Op::Sne
            | Op::Dsx
            | Op::Dsy
            | Op::Frc
            | Op::Pow => {
                let required_params = match op {
                    // dst + 1 src
                    Op::Mov
                    | Op::Exp
                    | Op::Log
                    | Op::Rcp
                    | Op::Rsq
                    | Op::Frc
                    | Op::Dsx
                    | Op::Dsy => 2,
                    // dst + 2 src
                    Op::Add
                    | Op::Sub
                    | Op::Mul
                    | Op::Min
                    | Op::Max
                    | Op::Slt
                    | Op::Sge
                    | Op::Seq
                    | Op::Sne
                    | Op::Dp2
                    | Op::Dp3
                    | Op::Dp4
                    | Op::Pow => 3,
                    // dst + 3 src
                    Op::Mad | Op::Lrp | Op::Cmp | Op::Dp2Add => 4,
                    _ => unreachable!("arithmetic op matched above"),
                };

                // The legacy translator only supports single-token operands. Reject token streams
                // whose instruction length doesn't match the opcode's fixed operand count (after
                // stripping the optional predicate token above).
                //
                // Accepting extra tokens would implicitly ignore multi-token operand encodings
                // like relative addressing, which can hide malformed bytecode behind legacy
                // fallback.
                if params.len() != required_params {
                    return Err(ShaderError::UnexpectedEof);
                }
                let dst = decode_dst(params[0])?;
                validate_dst_for_stage(stage, &dst)?;
                let src = params[1..required_params]
                    .iter()
                    .map(|t| decode_src(*t))
                    .collect::<Result<Vec<_>, _>>()?;
                for s in &src {
                    validate_src_for_stage(stage, s)?;
                }
                Instruction {
                    op,
                    dst: Some(dst),
                    src,
                    sampler: None,
                    imm: None,
                    result_modifier,
                }
            }
            Op::Texld => {
                // texld dst, coord, sampler
                if params.len() != 3 {
                    return Err(ShaderError::UnexpectedEof);
                }
                let dst = decode_dst(params[0])?;
                validate_dst_for_stage(stage, &dst)?;
                let coord = decode_src(params[1])?;
                validate_src_for_stage(stage, &coord)?;
                let sampler_src = decode_src(params[2])?;
                if sampler_src.reg.file != RegisterFile::Sampler {
                    return Err(ShaderError::InvalidSrcRegisterFile {
                        stage,
                        file: sampler_src.reg.file,
                    });
                }
                // Sampler operands are not numeric sources; reject any source modifier encodings
                // instead of silently ignoring them.
                let sampler_modifier_raw = ((params[2] >> 24) & 0xF) as u8;
                if sampler_modifier_raw != 0 {
                    return Err(ShaderError::UnsupportedSrcModifier(sampler_modifier_raw));
                };
                let sampler_index = sampler_src.reg.index;
                used_samplers.insert(sampler_index);
                Instruction {
                    op,
                    dst: Some(dst),
                    src: vec![coord],
                    sampler: Some(sampler_index),
                    imm: Some((token >> 16) & 0x1),
                    result_modifier,
                }
            }
            Op::End => unreachable!(),
        };

        // Track usage.
        if let Some(dst) = inst.dst {
            match dst.reg.file {
                RegisterFile::Temp => temp_max = temp_max.max(dst.reg.index + 1),
                RegisterFile::RastOut
                | RegisterFile::AttrOut
                | RegisterFile::TexCoordOut
                | RegisterFile::ColorOut => {
                    used_outputs.insert(dst.reg);
                }
                _ => {}
            }
        }
        for src in &inst.src {
            match src.reg.file {
                RegisterFile::Const => {
                    used_consts.insert(src.reg.index);
                }
                RegisterFile::Input | RegisterFile::Texture => {
                    used_inputs.insert(src.reg.index);
                }
                RegisterFile::Temp => temp_max = temp_max.max(src.reg.index + 1),
                RegisterFile::Sampler => {
                    used_samplers.insert(src.reg.index);
                }
                _ => {}
            }
        }

        instructions.push(inst);
    }

    if !if_stack.is_empty() {
        return Err(ShaderError::InvalidControlFlow("missing endif"));
    }

    // SM2/SM3 token streams are terminated by an explicit `end` instruction (opcode 0xFFFF). Treat
    // missing termination as malformed input rather than silently accepting truncated streams.
    if !saw_end {
        return Err(ShaderError::UnexpectedEof);
    }

    // Apply semantic-based vertex input remapping.
    //
    // D3D9's `dcl_*` declarations associate semantics with input registers (`v#`). The DX9 UMD is
    // free to assign semantics to arbitrary input registers (e.g. COLOR0 might be declared as
    // `v7`). Our WGSL backend uses `@location(v#)` for vertex inputs, so we remap those `v#`
    // indices to a canonical semantic-based location assignment. This makes vertex input binding
    // stable and ensures non-trivial register assignments receive the correct data.
    let mut uses_semantic_locations = false;
    let mut semantic_locations = Vec::<SemanticLocation>::new();
    if version.stage == ShaderStage::Vertex {
        let mut used_vs_inputs = BTreeSet::<u16>::new();
        for inst in &instructions {
            if let Some(dst) = inst.dst {
                if dst.reg.file == RegisterFile::Input {
                    used_vs_inputs.insert(dst.reg.index);
                }
            }
            for src in &inst.src {
                if src.reg.file == RegisterFile::Input {
                    used_vs_inputs.insert(src.reg.index);
                }
            }
        }

        if !used_vs_inputs.is_empty() {
            // Only enable semantic remapping when we have DCL declarations for all used input
            // registers. Otherwise, fall back to the legacy behavior (input index unchanged).
            let mut remap = HashMap::<u16, u16>::new();
            let mut used_locations = HashMap::<u32, u16>::new();
            let map = AdaptiveLocationMap::new(input_dcl_order.iter().copied())?;
            let mut can_remap = true;
            for &v in &used_vs_inputs {
                let Some(&(usage, usage_index)) = input_dcl_map.get(&v) else {
                    can_remap = false;
                    break;
                };
                let loc = map.location_for(usage, usage_index)?;
                if let Some(prev_v) = used_locations.insert(loc, v) {
                    if prev_v != v {
                        return Err(ShaderError::DuplicateInputLocation {
                            location: loc,
                            first: prev_v,
                            second: v,
                        });
                    }
                }
                remap.insert(v, loc as u16);
            }

            if can_remap {
                // Expose the semantic mapping used by this shader so the host can bind matching
                // vertex attributes.
                for (usage, usage_index) in input_dcl_order {
                    let location = map.location_for(usage, usage_index)?;
                    semantic_locations.push(SemanticLocation {
                        usage,
                        usage_index,
                        location,
                    });
                }

                for inst in &mut instructions {
                    if let Some(dst) = inst.dst.as_mut() {
                        if dst.reg.file == RegisterFile::Input {
                            if let Some(&new_idx) = remap.get(&dst.reg.index) {
                                dst.reg.index = new_idx;
                            }
                        }
                    }
                    for src in &mut inst.src {
                        if src.reg.file == RegisterFile::Input {
                            if let Some(&new_idx) = remap.get(&src.reg.index) {
                                src.reg.index = new_idx;
                            }
                        }
                    }
                }

                // Update the used-input set to reflect canonical locations.
                used_inputs = used_vs_inputs.iter().map(|v| remap[v]).collect();
                uses_semantic_locations = true;
            } else {
                // Keep `used_inputs` consistent with the instruction stream (but without semantic
                // remapping).
                used_inputs = used_vs_inputs;
            }
        }
    }

    Ok(ShaderProgram {
        version,
        instructions,
        const_defs_f32,
        const_defs_bool,
        used_samplers,
        sampler_texture_types,
        used_consts,
        used_inputs,
        used_outputs,
        temp_count: temp_max.max(1),
        uses_semantic_locations,
        semantic_locations,
    })
}

/// Parse DXBC or raw D3D9 shader bytecode into a [`ShaderProgram`].
pub fn parse(bytes: &[u8]) -> Result<ShaderProgram, ShaderError> {
    if bytes.len() > MAX_D3D9_SHADER_BLOB_BYTES {
        return Err(ShaderError::BytecodeTooLarge {
            len: bytes.len(),
            max: MAX_D3D9_SHADER_BLOB_BYTES,
        });
    }
    let token_stream = dxbc::extract_shader_bytecode(bytes)?;
    match parse_token_stream(token_stream) {
        Ok(program) => Ok(program),
        Err(err) => {
            // Some historical shader blobs encode opcode token length as the number of operand
            // tokens rather than the total instruction length. `parse_token_stream` expects the
            // SM2/SM3 spec's total-length encoding, so retry parsing after normalizing legacy
            // operand-count streams.
            let normalized =
                match crate::token_stream::normalize_sm2_sm3_instruction_lengths(token_stream) {
                    Ok(normalized) => normalized,
                    Err(_) => return Err(err),
                };

            // Avoid re-running the parser when normalization concluded that the stream already uses
            // total-length encoding.
            if matches!(normalized, std::borrow::Cow::Borrowed(_)) {
                return Err(err);
            }

            // Only accept the normalized parse result if it succeeds. Normalization is a best-effort
            // compatibility path for historical operand-count length encodings; for malformed or
            // ambiguous token streams it can produce less useful errors (e.g. consuming the final
            // `end` token as an operand). In those cases, preserve the original parser error.
            match parse_token_stream(normalized.as_ref()) {
                Ok(program) => Ok(program),
                Err(_) => Err(err),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderIr {
    pub version: ShaderVersion,
    pub temp_count: u16,
    pub ops: Vec<Instruction>,
    pub const_defs_f32: BTreeMap<u16, [f32; 4]>,
    pub const_defs_bool: BTreeMap<u16, bool>,
    pub used_samplers: BTreeSet<u16>,
    pub sampler_texture_types: HashMap<u16, TextureType>,
    pub used_consts: BTreeSet<u16>,
    pub used_inputs: BTreeSet<u16>,
    pub used_outputs: BTreeSet<Register>,
    pub uses_semantic_locations: bool,
    pub semantic_locations: Vec<SemanticLocation>,
}

pub fn to_ir(program: &ShaderProgram) -> ShaderIr {
    ShaderIr {
        version: program.version,
        temp_count: program.temp_count,
        ops: program.instructions.clone(),
        const_defs_f32: program.const_defs_f32.clone(),
        const_defs_bool: program.const_defs_bool.clone(),
        used_samplers: program.used_samplers.clone(),
        sampler_texture_types: program.sampler_texture_types.clone(),
        used_consts: program.used_consts.clone(),
        used_inputs: program.used_inputs.clone(),
        used_outputs: program.used_outputs.clone(),
        uses_semantic_locations: program.uses_semantic_locations,
        semantic_locations: program.semantic_locations.clone(),
    }
}

fn varying_location(reg: Register) -> Option<u32> {
    match reg.file {
        RegisterFile::AttrOut => Some(reg.index as u32),
        RegisterFile::TexCoordOut => Some(4 + reg.index as u32),
        _ => None,
    }
}

fn ps_input_location(reg: Register) -> Option<u32> {
    match reg.file {
        RegisterFile::Input => Some(reg.index as u32),
        RegisterFile::Texture => Some(4 + reg.index as u32),
        _ => None,
    }
}

fn reg_var_name(reg: Register) -> String {
    match reg.file {
        RegisterFile::Temp => format!("r{}", reg.index),
        RegisterFile::Input => format!("v{}", reg.index),
        RegisterFile::Const => format!("c{}", reg.index),
        RegisterFile::ConstBool => format!("b{}", reg.index),
        RegisterFile::Addr => format!("a{}", reg.index),
        RegisterFile::Texture => format!("t{}", reg.index),
        RegisterFile::Sampler => format!("s{}", reg.index),
        RegisterFile::RastOut => "oPos".to_string(),
        RegisterFile::AttrOut => format!("oD{}", reg.index),
        RegisterFile::TexCoordOut => format!("oT{}", reg.index),
        RegisterFile::ColorOut => format!("oC{}", reg.index),
    }
}

fn swizzle_suffix(swz: Swizzle) -> String {
    let comp = |c| match c {
        0 => 'x',
        1 => 'y',
        2 => 'z',
        3 => 'w',
        _ => 'x',
    };
    let chars: String = swz.0.into_iter().map(comp).collect();
    format!(".{}", chars)
}

#[derive(Debug, Clone)]
pub struct WgslOutput {
    pub wgsl: String,
    pub entry_point: &'static str,
    pub bind_group_layout: BindGroupLayout,
}

/// Options that affect the emitted WGSL *semantics*.
///
/// These must participate in any shader cache key derivation because toggling them changes the
/// generated WGSL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WgslOptions {
    /// When enabled, apply the classic D3D9 half-pixel center adjustment in the vertex shader.
    ///
    /// D3D9's viewport transform effectively subtracts 0.5 from the final window-space X/Y
    /// coordinate (see the "half-pixel offset" discussion in D3D9 docs / many D3D9->D3D10 porting
    /// guides). WebGPU follows the D3D10+ convention (no -0.5 bias), so we emulate D3D9 by
    /// translating clip-space XY by:
    ///
    ///   pos.xy += vec2(-1/viewport_width, +1/viewport_height) * pos.w
    ///
    /// This shifts the final rasterization by (-0.5, -0.5) pixels in window space.
    pub half_pixel_center: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindGroupLayout {
    /// Bind group index used for texture/sampler bindings in this shader stage.
    ///
    /// Contract:
    /// - group(0): constants shared by VS/PS (bindings 0/1/2 for float/int/bool constants)
    /// - group(1): VS texture/sampler bindings
    /// - group(2): PS texture/sampler bindings
    /// - group(3): optional half-pixel-center uniform buffer (VS only)
    pub sampler_group: u32,
    /// sampler_index -> (texture_binding, sampler_binding)
    pub sampler_bindings: HashMap<u16, (u32, u32)>,
}

pub fn generate_wgsl(ir: &ShaderIr) -> Result<WgslOutput, ShaderError> {
    generate_wgsl_with_options(ir, WgslOptions::default())
}

pub fn generate_wgsl_with_options(
    ir: &ShaderIr,
    options: WgslOptions,
) -> Result<WgslOutput, ShaderError> {
    let mut wgsl = String::new();

    // Shader constants: D3D9 has separate per-stage constant register files (VS=0..255, PS=256..511).
    //
    // Pack each register file into a stable per-type uniform buffer:
    // - binding(0): float4 constants (`c#`)
    // - binding(1): int4 constants (`i#`)
    // - binding(2): bool constants (`b#`, represented as `vec4<u32>` per register)
    wgsl.push_str("struct Constants { c: array<vec4<f32>, 512>, };\n");
    wgsl.push_str("struct ConstantsI { i: array<vec4<i32>, 512>, };\n");
    wgsl.push_str("struct ConstantsB { b: array<vec4<u32>, 512>, };\n");
    wgsl.push_str("@group(0) @binding(0) var<uniform> constants: Constants;\n");
    wgsl.push_str("@group(0) @binding(1) var<uniform> constants_i: ConstantsI;\n");
    wgsl.push_str("@group(0) @binding(2) var<uniform> constants_b: ConstantsB;\n\n");

    let sampler_group = match ir.version.stage {
        ShaderStage::Vertex => 1u32,
        ShaderStage::Pixel => 2u32,
    };
    let mut sampler_bindings = HashMap::new();
    // Allocate bindings: (texture, sampler) pairs.
    //
    // D3D9 has separate sampler register namespaces for vertex and pixel shaders. WebGPU binds
    // resources per (group, binding), so we model this by assigning each stage its own bind group:
    // - VS samplers live in group(1)
    // - PS samplers live in group(2)
    //
    // Derive binding numbers from the D3D9 sampler register index for stability:
    //   texture binding = 2*s
    //   sampler binding = 2*s + 1
    // If the shader declared an unsupported sampler texture type, reject before generating WGSL
    // to avoid `wgpu` validation errors from mismatched `texture_*` types / bind group layouts.
    for (&sampler, &ty) in &ir.sampler_texture_types {
        if !matches!(
            ty,
            TextureType::Texture1D
                | TextureType::Texture2D
                | TextureType::TextureCube
                | TextureType::Texture3D
        ) {
            return Err(ShaderError::UnsupportedSamplerTextureType {
                sampler: sampler as u32,
                ty,
            });
        }
    }
    for &s in &ir.used_samplers {
        let tex_binding = u32::from(s) * 2;
        let samp_binding = tex_binding + 1;
        sampler_bindings.insert(s, (tex_binding, samp_binding));
        let ty = ir
            .sampler_texture_types
            .get(&s)
            .copied()
            .unwrap_or(TextureType::Texture2D);
        let wgsl_tex_ty = match ty {
            TextureType::Texture1D => "texture_1d<f32>",
            TextureType::Texture2D => "texture_2d<f32>",
            TextureType::TextureCube => "texture_cube<f32>",
            TextureType::Texture3D => "texture_3d<f32>",
            _ => unreachable!("unsupported sampler texture types are rejected above"),
        };
        wgsl.push_str(&format!(
            "@group({}) @binding({}) var tex{}: {};\n",
            sampler_group, tex_binding, s, wgsl_tex_ty
        ));
        wgsl.push_str(&format!(
            "@group({}) @binding({}) var samp{}: sampler;\n",
            sampler_group, samp_binding, s
        ));
    }
    if !ir.used_samplers.is_empty() {
        wgsl.push('\n');
    }

    if ir.version.stage == ShaderStage::Vertex && options.half_pixel_center {
        // Separate bind group so the half-pixel fix is opt-in and cache-keyed.
        //
        // NOTE: group(1) and group(2) are reserved for VS/PS sampler bindings respectively, so the
        // half-pixel uniform lives in group(3).
        wgsl.push_str("struct HalfPixel { inv_viewport: vec2<f32>, _pad: vec2<f32>, };\n");
        wgsl.push_str("@group(3) @binding(0) var<uniform> half_pixel: HalfPixel;\n\n");
    }

    let const_base = match ir.version.stage {
        ShaderStage::Vertex => 0u32,
        ShaderStage::Pixel => 256u32,
    };

    // D3D9 boolean constants (`b#`).
    //
    // Notes:
    // - D3D9 bool regs are scalar; we splat them across vec4 for register-like access with swizzles.
    // - `defb` values override the uniform constant buffer.
    // - Non-embedded `b#` values are loaded from the host-updated uniform buffer.
    let mut used_bool_consts = BTreeSet::<u16>::new();
    for inst in &ir.ops {
        if let Some(dst) = inst.dst {
            if dst.reg.file == RegisterFile::ConstBool {
                used_bool_consts.insert(dst.reg.index);
            }
        }
        for src in &inst.src {
            if src.reg.file == RegisterFile::ConstBool {
                used_bool_consts.insert(src.reg.index);
            }
        }
    }

    match ir.version.stage {
        ShaderStage::Vertex => {
            let has_inputs = !ir.used_inputs.is_empty();
            if has_inputs {
                // Input struct.
                wgsl.push_str("struct VsInput {\n");
                for &v in &ir.used_inputs {
                    wgsl.push_str(&format!("  @location({}) v{}: vec4<f32>,\n", v, v));
                }
                wgsl.push_str("};\n");
            }

            // Output struct.
            wgsl.push_str("struct VsOutput {\n  @builtin(position) pos: vec4<f32>,\n");
            for &reg in &ir.used_outputs {
                if reg.file == RegisterFile::RastOut {
                    continue;
                }
                if let Some(loc) = varying_location(reg) {
                    wgsl.push_str(&format!(
                        "  @location({}) {}: vec4<f32>,\n",
                        loc,
                        reg_var_name(reg)
                    ));
                }
            }
            wgsl.push_str("};\n\n");

            if has_inputs {
                wgsl.push_str("@vertex\nfn vs_main(input: VsInput) -> VsOutput {\n");
            } else {
                wgsl.push_str("@vertex\nfn vs_main() -> VsOutput {\n");
            }
            // Declare registers.
            for i in 0..ir.temp_count {
                wgsl.push_str(&format!("  var r{}: vec4<f32> = vec4<f32>(0.0);\n", i));
            }
            for b in &used_bool_consts {
                if let Some(v) = ir.const_defs_bool.get(b).copied() {
                    let v = if v { "1.0" } else { "0.0" };
                    wgsl.push_str(&format!("  let b{}: vec4<f32> = vec4<f32>({v});\n", b));
                } else {
                    wgsl.push_str(&format!(
                        "  let b{}: vec4<f32> = vec4<f32>(select(0.0, 1.0, constants_b.b[{}u + {}u].x != 0u));\n",
                        b, const_base, b
                    ));
                }
            }
            for &v in &ir.used_inputs {
                wgsl.push_str(&format!("  let v{}: vec4<f32> = input.v{};\n", v, v));
            }
            for (&idx, val) in &ir.const_defs_f32 {
                wgsl.push_str(&format!(
                    "  let c{}: vec4<f32> = vec4<f32>({}, {}, {}, {});\n",
                    idx,
                    wgsl_f32(val[0]),
                    wgsl_f32(val[1]),
                    wgsl_f32(val[2]),
                    wgsl_f32(val[3])
                ));
            }
            // Outputs.
            wgsl.push_str("  var oPos: vec4<f32> = vec4<f32>(0.0);\n");
            for &reg in &ir.used_outputs {
                match reg.file {
                    RegisterFile::AttrOut | RegisterFile::TexCoordOut => {
                        wgsl.push_str(&format!(
                            "  var {}: vec4<f32> = vec4<f32>(0.0);\n",
                            reg_var_name(reg)
                        ));
                    }
                    _ => {}
                }
            }
            wgsl.push('\n');

            // Instruction emission.
            let mut indent = 1usize;
            for inst in &ir.ops {
                emit_inst(
                    &mut wgsl,
                    &mut indent,
                    inst,
                    &ir.const_defs_f32,
                    const_base,
                    ir.version.stage,
                    &ir.sampler_texture_types,
                );
            }
            debug_assert_eq!(indent, 1, "unbalanced if/endif indentation");

            wgsl.push_str("  var out: VsOutput;\n  out.pos = oPos;\n");
            if options.half_pixel_center {
                wgsl.push_str(
                    "  // D3D9 half-pixel center adjustment: emulate the D3D9 viewport transform's\n  // -0.5 window-space bias by nudging clip-space XY by (-1/width, +1/height) * w.\n",
                );
                wgsl.push_str("  out.pos.x = out.pos.x - half_pixel.inv_viewport.x * out.pos.w;\n");
                wgsl.push_str("  out.pos.y = out.pos.y + half_pixel.inv_viewport.y * out.pos.w;\n");
            }
            for &reg in &ir.used_outputs {
                if reg.file == RegisterFile::RastOut {
                    continue;
                }
                if varying_location(reg).is_some() {
                    wgsl.push_str(&format!(
                        "  out.{} = {};\n",
                        reg_var_name(reg),
                        reg_var_name(reg)
                    ));
                }
            }
            wgsl.push_str("  return out;\n}\n");

            Ok(WgslOutput {
                wgsl,
                entry_point: "vs_main",
                bind_group_layout: BindGroupLayout {
                    sampler_group,
                    sampler_bindings,
                },
            })
        }
        ShaderStage::Pixel => {
            // Inputs are driven by varying mapping. We just emit for any used input regs.
            // For simplicity we emit `v#` as @location(#) and `t#` as @location(4+#).
            let mut inputs_by_reg = BTreeSet::new();
            for inst in &ir.ops {
                for src in &inst.src {
                    match src.reg.file {
                        RegisterFile::Input | RegisterFile::Texture => {
                            inputs_by_reg.insert(src.reg);
                        }
                        _ => {}
                    }
                }
            }

            let has_inputs = !inputs_by_reg.is_empty();
            let mut color_outputs = BTreeSet::<u16>::new();
            for &reg in &ir.used_outputs {
                if reg.file == RegisterFile::ColorOut {
                    color_outputs.insert(reg.index);
                }
            }
            // D3D9 pixel shaders conceptually write at least oC0. Keep the generated WGSL stable
            // by always emitting location(0), even if the shader bytecode never assigns it.
            color_outputs.insert(0);

            wgsl.push_str("struct PsOutput {\n");
            for &idx in &color_outputs {
                let reg = Register {
                    file: RegisterFile::ColorOut,
                    index: idx,
                };
                wgsl.push_str(&format!(
                    "  @location({}) {}: vec4<f32>,\n",
                    idx,
                    reg_var_name(reg)
                ));
            }
            wgsl.push_str("};\n\n");

            if has_inputs {
                wgsl.push_str("struct PsInput {\n");
                for reg in &inputs_by_reg {
                    if let Some(loc) = ps_input_location(*reg) {
                        wgsl.push_str(&format!(
                            "  @location({}) {}: vec4<f32>,\n",
                            loc,
                            reg_var_name(*reg)
                        ));
                    }
                }
                wgsl.push_str("};\n\n");
                wgsl.push_str("@fragment\nfn fs_main(input: PsInput) -> PsOutput {\n");
            } else {
                // WGSL does not permit empty structs, so if the shader uses no varyings we
                // omit the input parameter entirely.
                wgsl.push_str("@fragment\nfn fs_main() -> PsOutput {\n");
            }
            for i in 0..ir.temp_count {
                wgsl.push_str(&format!("  var r{}: vec4<f32> = vec4<f32>(0.0);\n", i));
            }
            for b in &used_bool_consts {
                if let Some(v) = ir.const_defs_bool.get(b).copied() {
                    let v = if v { "1.0" } else { "0.0" };
                    wgsl.push_str(&format!("  let b{}: vec4<f32> = vec4<f32>({v});\n", b));
                } else {
                    wgsl.push_str(&format!(
                        "  let b{}: vec4<f32> = vec4<f32>(select(0.0, 1.0, constants_b.b[{}u + {}u].x != 0u));\n",
                        b, const_base, b
                    ));
                }
            }
            // Load inputs.
            if has_inputs {
                for reg in &inputs_by_reg {
                    wgsl.push_str(&format!(
                        "  let {}: vec4<f32> = input.{};\n",
                        reg_var_name(*reg),
                        reg_var_name(*reg)
                    ));
                }
            }
            for (&idx, val) in &ir.const_defs_f32 {
                wgsl.push_str(&format!(
                    "  let c{}: vec4<f32> = vec4<f32>({}, {}, {}, {});\n",
                    idx,
                    wgsl_f32(val[0]),
                    wgsl_f32(val[1]),
                    wgsl_f32(val[2]),
                    wgsl_f32(val[3])
                ));
            }
            for &idx in &color_outputs {
                wgsl.push_str(&format!("  var oC{}: vec4<f32> = vec4<f32>(0.0);\n", idx));
            }
            wgsl.push('\n');

            let mut indent = 1usize;
            let mut skip_ops = BTreeSet::<usize>::new();
            let mut i = 0usize;
            while i < ir.ops.len() {
                if skip_ops.remove(&i) {
                    i += 1;
                    continue;
                }

                if let Some(next_i) = try_emit_uniform_control_flow_if_predicated_block(
                    &mut wgsl,
                    indent,
                    &ir.ops,
                    i,
                    &ir.const_defs_f32,
                    const_base,
                    ir.version.stage,
                    &ir.sampler_texture_types,
                ) {
                    i = next_i;
                    continue;
                }

                // If the `else` block begins with a uniformity-sensitive op, hoist it out of the
                // branch by emitting it as a conditional `select` assignment *before* the `if`.
                //
                // This handles patterns like:
                //   if (...) { ... } else { texld ...; ... }
                if matches!(ir.ops[i].op, Op::If | Op::Ifc) {
                    if let Some(cond) =
                        if_condition_expr(&ir.ops[i], &ir.const_defs_f32, const_base)
                    {
                        if let Some(else_op_idx) = find_else_first_op_index(&ir.ops, i) {
                            if matches!(ir.ops[else_op_idx].op, Op::Texld | Op::Dsx | Op::Dsy) {
                                let pred = format!("!({cond})");
                                if try_emit_uniform_control_flow_predicated_op_assignment(
                                    &mut wgsl,
                                    indent,
                                    &pred,
                                    &ir.ops[else_op_idx],
                                    &ir.const_defs_f32,
                                    const_base,
                                    ir.version.stage,
                                    &ir.sampler_texture_types,
                                ) {
                                    skip_ops.insert(else_op_idx);
                                }
                            }
                        }
                    }
                }

                if i + 2 < ir.ops.len()
                    && matches!(ir.ops[i].op, Op::If | Op::Ifc)
                    && matches!(ir.ops[i + 2].op, Op::EndIf)
                    && matches!(ir.ops[i + 1].op, Op::Texld | Op::Dsx | Op::Dsy)
                    && try_emit_uniform_control_flow_if_single_op(
                        &mut wgsl,
                        indent,
                        &ir.ops[i],
                        &ir.ops[i + 1],
                        &ir.ops[i + 2],
                        &ir.const_defs_f32,
                        const_base,
                        ir.version.stage,
                        &ir.sampler_texture_types,
                    )
                {
                    i += 3;
                    continue;
                }

                // If the first instruction inside the `if` block is a derivative op (`dsx`/`dsy`)
                // or implicit-derivative texture sample (`texld`), emit it outside the `if` via
                // `select` and keep the remaining control flow intact. This covers multi-statement
                // blocks where only the first op needs to be in uniform control flow.
                if i + 1 < ir.ops.len()
                    && matches!(ir.ops[i].op, Op::If | Op::Ifc)
                    && matches!(ir.ops[i + 1].op, Op::Texld | Op::Dsx | Op::Dsy)
                {
                    if let Some(cond) =
                        if_condition_expr(&ir.ops[i], &ir.const_defs_f32, const_base)
                    {
                        if try_emit_uniform_control_flow_predicated_op_assignment(
                            &mut wgsl,
                            indent,
                            &cond,
                            &ir.ops[i + 1],
                            &ir.const_defs_f32,
                            const_base,
                            ir.version.stage,
                            &ir.sampler_texture_types,
                        ) {
                            emit_inst(
                                &mut wgsl,
                                &mut indent,
                                &ir.ops[i],
                                &ir.const_defs_f32,
                                const_base,
                                ir.version.stage,
                                &ir.sampler_texture_types,
                            );
                            // Skip the hoisted op inside the `if` block.
                            i += 2;
                            continue;
                        }
                    }
                }
                emit_inst(
                    &mut wgsl,
                    &mut indent,
                    &ir.ops[i],
                    &ir.const_defs_f32,
                    const_base,
                    ir.version.stage,
                    &ir.sampler_texture_types,
                );
                i += 1;
            }
            debug_assert_eq!(indent, 1, "unbalanced if/endif indentation");
            wgsl.push_str("  var out: PsOutput;\n");
            for &idx in &color_outputs {
                wgsl.push_str(&format!("  out.oC{} = oC{};\n", idx, idx));
            }
            wgsl.push_str("  return out;\n}\n");

            Ok(WgslOutput {
                wgsl,
                entry_point: "fs_main",
                bind_group_layout: BindGroupLayout {
                    sampler_group,
                    sampler_bindings,
                },
            })
        }
    }
}

fn push_indent(wgsl: &mut String, indent: usize) {
    for _ in 0..indent {
        wgsl.push_str("  ");
    }
}

fn emit_assign(wgsl: &mut String, indent: usize, dst: Dst, value: &str) {
    // WGSL does not permit assignment to multi-component swizzles (e.g. `v.xy = ...`), so lower
    // write masks to per-component assignments.
    //
    // Note: single-component assignments (`v.x = ...`) are permitted.
    if dst.mask.0 == 0 {
        return;
    }

    let dst_name = reg_var_name(dst.reg);
    if dst.mask == WriteMask::XYZW {
        push_indent(wgsl, indent);
        wgsl.push_str(&format!("{dst_name} = {value};\n"));
        return;
    }

    let mut comps = Vec::new();
    if dst.mask.write_x() {
        comps.push('x');
    }
    if dst.mask.write_y() {
        comps.push('y');
    }
    if dst.mask.write_z() {
        comps.push('z');
    }
    if dst.mask.write_w() {
        comps.push('w');
    }

    if comps.len() == 1 {
        let c = comps[0];
        push_indent(wgsl, indent);
        wgsl.push_str(&format!("{dst_name}.{c} = ({value}).{c};\n"));
        return;
    }

    push_indent(wgsl, indent);
    wgsl.push_str("{ let _tmp = ");
    wgsl.push_str(value);
    wgsl.push_str("; ");
    for c in comps {
        wgsl.push_str(&format!("{dst_name}.{c} = _tmp.{c}; "));
    }
    wgsl.push_str("}\n");
}

fn texld_sample_expr(
    inst: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> String {
    let coord = inst.src[0];
    let s = inst.sampler.unwrap_or(0);
    let coord_expr = src_expr(&coord, const_defs_f32, const_base);
    let project = inst.imm.unwrap_or(0) != 0;
    let ty = sampler_texture_types
        .get(&s)
        .copied()
        .unwrap_or(TextureType::Texture2D);
    let coords = match ty {
        TextureType::TextureCube | TextureType::Texture3D => {
            if project {
                format!("(({}).xyz / ({}).w)", coord_expr, coord_expr)
            } else {
                format!("({}).xyz", coord_expr)
            }
        }
        TextureType::Texture2D => {
            if project {
                format!("(({}).xy / ({}).w)", coord_expr, coord_expr)
            } else {
                format!("({}).xy", coord_expr)
            }
        }
        TextureType::Texture1D => {
            if project {
                format!("(({}).x / ({}).w)", coord_expr, coord_expr)
            } else {
                format!("({}).x", coord_expr)
            }
        }
        _ => {
            unreachable!("unsupported sampler texture types are rejected during WGSL generation")
        }
    };

    let sample = match stage {
        // Vertex stage has no implicit derivatives, so use an explicit LOD.
        ShaderStage::Vertex => format!("textureSampleLevel(tex{}, samp{}, {}, 0.0)", s, s, coords),
        ShaderStage::Pixel => format!("textureSample(tex{}, samp{}, {})", s, s, coords),
    };
    apply_result_modifier(sample, inst.result_modifier)
}

fn derivative_expr(
    inst: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    op: Op,
) -> String {
    let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
    let expr = match op {
        Op::Dsx => format!("dpdx({})", src0),
        Op::Dsy => format!("dpdy({})", src0),
        _ => unreachable!("derivative_expr called for non-derivative op"),
    };
    apply_result_modifier(expr, inst.result_modifier)
}

fn if_condition_expr(
    inst: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
) -> Option<String> {
    match inst.op {
        Op::If => {
            let cond = src_expr(&inst.src[0], const_defs_f32, const_base);
            Some(format!("({}).x != 0.0", cond))
        }
        Op::Ifc => {
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let op = match inst.imm.unwrap_or(0) {
                0 => ">",
                1 => "==",
                2 => ">=",
                3 => "<",
                4 => "!=",
                5 => "<=",
                _ => "==",
            };
            Some(format!("({}).x {} ({}).x", a, op, b))
        }
        _ => None,
    }
}

fn find_else_first_op_index(ops: &[Instruction], if_index: usize) -> Option<usize> {
    // Walk forward until we find the matching `else` (if any) for the `if` at `if_index`.
    // Track nested `if` depth so we ignore inner `else` tokens.
    let mut depth = 0usize;
    for (idx, inst) in ops.iter().enumerate().skip(if_index + 1) {
        match inst.op {
            Op::If | Op::Ifc => depth += 1,
            Op::EndIf => {
                if depth == 0 {
                    // Reached the end of the current `if` without seeing an `else`.
                    return None;
                }
                depth -= 1;
            }
            Op::Else if depth == 0 => {
                return ops.get(idx + 1).map(|_| idx + 1);
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, Copy)]
struct IfBounds {
    else_idx: Option<usize>,
    endif_idx: usize,
}

fn find_if_bounds(ops: &[Instruction], if_index: usize) -> Option<IfBounds> {
    // Walk forward until we find the matching `endif` for the `if` at `if_index`. Track nested `if`
    // depth so we ignore inner `else`/`endif` tokens.
    let mut depth = 0usize;
    let mut else_idx = None;
    for (idx, inst) in ops.iter().enumerate().skip(if_index + 1) {
        match inst.op {
            Op::If | Op::Ifc => depth += 1,
            Op::EndIf => {
                if depth == 0 {
                    return Some(IfBounds {
                        else_idx,
                        endif_idx: idx,
                    });
                }
                depth -= 1;
            }
            Op::Else if depth == 0 => {
                else_idx = Some(idx);
            }
            _ => {}
        }
    }
    None
}

fn is_uniformity_sensitive_op(op: Op) -> bool {
    matches!(op, Op::Texld | Op::Dsx | Op::Dsy)
}

fn branch_has_uniformity_sensitive_op_not_first(
    ops: &[Instruction],
    start: usize,
    end: usize,
) -> bool {
    if start >= end {
        return false;
    }
    ops.iter()
        .enumerate()
        .take(end)
        .skip(start + 1)
        .any(|(_, inst)| is_uniformity_sensitive_op(inst.op))
}

fn inst_value_expr(
    inst: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> Option<String> {
    let expr = match inst.op {
        Op::Mov => {
            let src0 = inst.src.first()?;
            Some(apply_result_modifier(
                src_expr(src0, const_defs_f32, const_base),
                inst.result_modifier,
            ))
        }
        Op::Add | Op::Sub | Op::Mul => {
            let src0 = inst.src.first()?;
            let src1 = inst.src.get(1)?;
            let op = match inst.op {
                Op::Add => "+",
                Op::Sub => "-",
                Op::Mul => "*",
                _ => unreachable!(),
            };
            Some(apply_result_modifier(
                format!(
                    "({} {} {})",
                    src_expr(src0, const_defs_f32, const_base),
                    op,
                    src_expr(src1, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Min | Op::Max => {
            let src0 = inst.src.first()?;
            let src1 = inst.src.get(1)?;
            let func = if inst.op == Op::Min { "min" } else { "max" };
            Some(apply_result_modifier(
                format!(
                    "{}({}, {})",
                    func,
                    src_expr(src0, const_defs_f32, const_base),
                    src_expr(src1, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Mad => {
            let a = inst.src.first()?;
            let b = inst.src.get(1)?;
            let c = inst.src.get(2)?;
            Some(apply_result_modifier(
                format!(
                    "fma({}, {}, {})",
                    src_expr(a, const_defs_f32, const_base),
                    src_expr(b, const_defs_f32, const_base),
                    src_expr(c, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Dp2Add => {
            let a = inst.src.first()?;
            let b = inst.src.get(1)?;
            let c = inst.src.get(2)?;
            let a = src_expr(a, const_defs_f32, const_base);
            let b = src_expr(b, const_defs_f32, const_base);
            let c = src_expr(c, const_defs_f32, const_base);
            Some(apply_result_modifier(
                format!("vec4<f32>(dot(({a}).xy, ({b}).xy) + ({c}).x)"),
                inst.result_modifier,
            ))
        }
        Op::Lrp => {
            let t = inst.src.first()?;
            let a = inst.src.get(1)?;
            let b = inst.src.get(2)?;
            // D3D9 `lrp`: dst = t * a + (1 - t) * b = mix(b, a, t).
            Some(apply_result_modifier(
                format!(
                    "mix({}, {}, {})",
                    src_expr(b, const_defs_f32, const_base),
                    src_expr(a, const_defs_f32, const_base),
                    src_expr(t, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Cmp => {
            let cond = inst.src.first()?;
            let a = inst.src.get(1)?;
            let b = inst.src.get(2)?;
            // Per-component compare: if cond >= 0 then a else b.
            Some(apply_result_modifier(
                format!(
                    "select({}, {}, ({} >= vec4<f32>(0.0)))",
                    src_expr(b, const_defs_f32, const_base),
                    src_expr(a, const_defs_f32, const_base),
                    src_expr(cond, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Slt | Op::Sge | Op::Seq | Op::Sne => {
            let a = inst.src.first()?;
            let b = inst.src.get(1)?;
            let op = match inst.op {
                Op::Slt => "<",
                Op::Sge => ">=",
                Op::Seq => "==",
                Op::Sne => "!=",
                _ => unreachable!(),
            };
            Some(apply_result_modifier(
                format!(
                    "select(vec4<f32>(0.0), vec4<f32>(1.0), ({} {} {}))",
                    src_expr(a, const_defs_f32, const_base),
                    op,
                    src_expr(b, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Dp2 | Op::Dp3 | Op::Dp4 => {
            let a = inst.src.first()?;
            let b = inst.src.get(1)?;
            let a = src_expr(a, const_defs_f32, const_base);
            let b = src_expr(b, const_defs_f32, const_base);
            let expr = match inst.op {
                Op::Dp2 => format!("vec4<f32>(dot(({a}).xy, ({b}).xy))"),
                Op::Dp3 => format!("vec4<f32>(dot(({a}).xyz, ({b}).xyz))"),
                Op::Dp4 => format!("vec4<f32>(dot({a}, {b}))"),
                _ => unreachable!(),
            };
            Some(apply_result_modifier(expr, inst.result_modifier))
        }
        Op::Texld => Some(texld_sample_expr(
            inst,
            const_defs_f32,
            const_base,
            stage,
            sampler_texture_types,
        )),
        Op::Rcp => {
            let src0 = inst.src.first()?;
            Some(apply_result_modifier(
                format!(
                    "(vec4<f32>(1.0) / {})",
                    src_expr(src0, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Rsq => {
            let src0 = inst.src.first()?;
            Some(apply_result_modifier(
                format!(
                    "inverseSqrt({})",
                    src_expr(src0, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Exp => {
            let src0 = inst.src.first()?;
            Some(apply_result_modifier(
                format!("exp2({})", src_expr(src0, const_defs_f32, const_base)),
                inst.result_modifier,
            ))
        }
        Op::Log => {
            let src0 = inst.src.first()?;
            Some(apply_result_modifier(
                format!("log2({})", src_expr(src0, const_defs_f32, const_base)),
                inst.result_modifier,
            ))
        }
        Op::Dsx | Op::Dsy if stage == ShaderStage::Pixel => {
            Some(derivative_expr(inst, const_defs_f32, const_base, inst.op))
        }
        Op::Pow => {
            let src0 = inst.src.first()?;
            let src1 = inst.src.get(1)?;
            Some(apply_result_modifier(
                format!(
                    "pow({}, {})",
                    src_expr(src0, const_defs_f32, const_base),
                    src_expr(src1, const_defs_f32, const_base)
                ),
                inst.result_modifier,
            ))
        }
        Op::Frc => {
            let src0 = inst.src.first()?;
            Some(apply_result_modifier(
                format!("fract({})", src_expr(src0, const_defs_f32, const_base)),
                inst.result_modifier,
            ))
        }
        _ => None,
    }?;
    Some(expr)
}

#[allow(clippy::too_many_arguments)]
fn try_emit_predicated_assignment(
    wgsl: &mut String,
    indent: usize,
    pred: &str,
    inst: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> bool {
    let dst = match inst.dst {
        Some(dst) => dst,
        None => return false,
    };
    let new_value = match inst_value_expr(
        inst,
        const_defs_f32,
        const_base,
        stage,
        sampler_texture_types,
    ) {
        Some(v) => v,
        None => return false,
    };

    let dst_name = reg_var_name(dst.reg);
    let expr = format!("select({dst_name}, {new_value}, {pred})");
    emit_assign(wgsl, indent, dst, &expr);
    true
}

#[allow(clippy::too_many_arguments)]
fn try_emit_predicated_ops_range(
    wgsl: &mut String,
    indent: usize,
    ops: &[Instruction],
    start: usize,
    end: usize,
    pred: &str,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> bool {
    let mut i = start;
    while i < end {
        let inst = &ops[i];
        match inst.op {
            Op::Nop => {
                i += 1;
            }
            Op::If | Op::Ifc => {
                let bounds = match find_if_bounds(ops, i) {
                    Some(b) => b,
                    None => return false,
                };
                if bounds.endif_idx >= end {
                    return false;
                }
                let cond = match if_condition_expr(inst, const_defs_f32, const_base) {
                    Some(cond) => cond,
                    None => return false,
                };

                let then_start = i + 1;
                let then_end = bounds.else_idx.unwrap_or(bounds.endif_idx);
                let then_pred = format!("({pred}) && ({cond})");
                if !try_emit_predicated_ops_range(
                    wgsl,
                    indent,
                    ops,
                    then_start,
                    then_end,
                    &then_pred,
                    const_defs_f32,
                    const_base,
                    stage,
                    sampler_texture_types,
                ) {
                    return false;
                }

                if let Some(else_idx) = bounds.else_idx {
                    let else_pred = format!("({pred}) && !({cond})");
                    if !try_emit_predicated_ops_range(
                        wgsl,
                        indent,
                        ops,
                        else_idx + 1,
                        bounds.endif_idx,
                        &else_pred,
                        const_defs_f32,
                        const_base,
                        stage,
                        sampler_texture_types,
                    ) {
                        return false;
                    }
                }

                i = bounds.endif_idx + 1;
            }
            Op::Else | Op::EndIf => return false,
            _ => {
                if !try_emit_predicated_assignment(
                    wgsl,
                    indent,
                    pred,
                    inst,
                    const_defs_f32,
                    const_base,
                    stage,
                    sampler_texture_types,
                ) {
                    return false;
                }
                i += 1;
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn try_emit_uniform_control_flow_if_predicated_block(
    wgsl: &mut String,
    indent: usize,
    ops: &[Instruction],
    if_index: usize,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> Option<usize> {
    if stage != ShaderStage::Pixel {
        return None;
    }
    let if_inst = ops.get(if_index)?;
    if !matches!(if_inst.op, Op::If | Op::Ifc) {
        return None;
    }

    let bounds = find_if_bounds(ops, if_index)?;

    let then_start = if_index + 1;
    let then_end = bounds.else_idx.unwrap_or(bounds.endif_idx);
    if then_end > ops.len() {
        return None;
    }
    let else_start = bounds.else_idx.map(|idx| idx + 1);

    // Only rewrite when a uniformity-sensitive op appears somewhere other than the first op of its
    // branch. These cases cannot be fixed by simply hoisting a single op without reordering the
    // non-uniform branch contents.
    let needs_predication = branch_has_uniformity_sensitive_op_not_first(ops, then_start, then_end)
        || else_start.is_some_and(|start| {
            branch_has_uniformity_sensitive_op_not_first(ops, start, bounds.endif_idx)
        });
    if !needs_predication {
        return None;
    }

    // Ensure there is at least one uniformity-sensitive op in this statement; otherwise keep the
    // original control flow intact.
    let has_uniformity_sensitive = ops
        .iter()
        .take(bounds.endif_idx)
        .skip(then_start)
        .any(|inst| is_uniformity_sensitive_op(inst.op));
    if !has_uniformity_sensitive {
        return None;
    }

    let cond = if_condition_expr(if_inst, const_defs_f32, const_base)?;

    // Emit into a temporary buffer so we don't partially rewrite on failure.
    let mut tmp = String::new();
    if !try_emit_predicated_ops_range(
        &mut tmp,
        indent,
        ops,
        then_start,
        then_end,
        &cond,
        const_defs_f32,
        const_base,
        stage,
        sampler_texture_types,
    ) {
        return None;
    }
    if let Some(else_idx) = bounds.else_idx {
        let pred = format!("!({cond})");
        if !try_emit_predicated_ops_range(
            &mut tmp,
            indent,
            ops,
            else_idx + 1,
            bounds.endif_idx,
            &pred,
            const_defs_f32,
            const_base,
            stage,
            sampler_texture_types,
        ) {
            return None;
        }
    }

    wgsl.push_str(&tmp);
    Some(bounds.endif_idx + 1)
}

#[allow(clippy::too_many_arguments)]
fn try_emit_uniform_control_flow_predicated_op_assignment(
    wgsl: &mut String,
    indent: usize,
    pred: &str,
    op: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> bool {
    if stage != ShaderStage::Pixel {
        return false;
    }
    let dst = match op.dst {
        Some(dst) => dst,
        None => return false,
    };
    let new_value = match op.op {
        Op::Dsx | Op::Dsy => derivative_expr(op, const_defs_f32, const_base, op.op),
        Op::Texld => {
            texld_sample_expr(op, const_defs_f32, const_base, stage, sampler_texture_types)
        }
        _ => return false,
    };

    let dst_name = reg_var_name(dst.reg);
    let expr = format!("select({dst_name}, {new_value}, {pred})");
    emit_assign(wgsl, indent, dst, &expr);
    true
}

#[allow(clippy::too_many_arguments)]
fn try_emit_uniform_control_flow_if_single_op(
    wgsl: &mut String,
    indent: usize,
    if_inst: &Instruction,
    body: &Instruction,
    endif: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> bool {
    // WGSL derivative ops (`dpdx`/`dpdy`) and implicit-derivative texture sampling (`textureSample`)
    // must be in uniform control flow. A common D3D9 pattern is `if (cond) { <single op>; }`
    // where `cond` depends on per-pixel values, which would produce invalid WGSL.
    //
    // For these ops, lower to unconditional evaluation + conditional assignment via `select`, which
    // does not introduce control flow.
    if stage != ShaderStage::Pixel {
        return false;
    }
    if !matches!(endif.op, Op::EndIf) {
        return false;
    }
    let cond = match if_condition_expr(if_inst, const_defs_f32, const_base) {
        Some(cond) => cond,
        None => return false,
    };
    try_emit_uniform_control_flow_predicated_op_assignment(
        wgsl,
        indent,
        &cond,
        body,
        const_defs_f32,
        const_base,
        stage,
        sampler_texture_types,
    )
}

fn emit_inst(
    wgsl: &mut String,
    indent: &mut usize,
    inst: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
    sampler_texture_types: &HashMap<u16, TextureType>,
) {
    match inst.op {
        Op::Nop => {}
        Op::End => {}
        Op::Mov => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let mut expr = src_expr(&src0, const_defs_f32, const_base);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Add | Op::Sub | Op::Mul => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let src1 = inst.src[1];
            let op = match inst.op {
                Op::Add => "+",
                Op::Sub => "-",
                Op::Mul => "*",
                _ => unreachable!(),
            };
            let mut expr = format!(
                "({} {} {})",
                src_expr(&src0, const_defs_f32, const_base),
                op,
                src_expr(&src1, const_defs_f32, const_base)
            );
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Min | Op::Max => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let src1 = inst.src[1];
            let func = if inst.op == Op::Min { "min" } else { "max" };
            let mut expr = format!(
                "{}({}, {})",
                func,
                src_expr(&src0, const_defs_f32, const_base),
                src_expr(&src1, const_defs_f32, const_base)
            );
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Mad => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let c = src_expr(&inst.src[2], const_defs_f32, const_base);
            let mut expr = format!("fma({}, {}, {})", a, b, c);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Dp2Add => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let c = src_expr(&inst.src[2], const_defs_f32, const_base);
            let mut expr = format!("vec4<f32>(dot(({a}).xy, ({b}).xy) + ({c}).x)");
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Lrp => {
            let dst = inst.dst.unwrap();
            let t = src_expr(&inst.src[0], const_defs_f32, const_base);
            let a = src_expr(&inst.src[1], const_defs_f32, const_base);
            let b = src_expr(&inst.src[2], const_defs_f32, const_base);
            // D3D9 `lrp`: dst = t * a + (1 - t) * b = mix(b, a, t).
            let mut expr = format!("mix({}, {}, {})", b, a, t);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Cmp => {
            let dst = inst.dst.unwrap();
            let cond = src_expr(&inst.src[0], const_defs_f32, const_base);
            let a = src_expr(&inst.src[1], const_defs_f32, const_base);
            let b = src_expr(&inst.src[2], const_defs_f32, const_base);
            // Per-component compare: if cond >= 0 then a else b.
            let mut expr = format!("select({}, {}, ({} >= vec4<f32>(0.0)))", b, a, cond);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Slt | Op::Sge | Op::Seq | Op::Sne => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let op = match inst.op {
                Op::Slt => "<",
                Op::Sge => ">=",
                Op::Seq => "==",
                Op::Sne => "!=",
                _ => unreachable!(),
            };
            let mut expr = format!(
                "select(vec4<f32>(0.0), vec4<f32>(1.0), ({} {} {}))",
                a, op, b
            );
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Dp2 | Op::Dp3 | Op::Dp4 => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let mut expr = match inst.op {
                Op::Dp2 => format!("vec4<f32>(dot(({}).xy, ({}).xy))", a, b),
                Op::Dp3 => format!("vec4<f32>(dot(({}).xyz, ({}).xyz))", a, b),
                Op::Dp4 => format!("vec4<f32>(dot({}, {}))", a, b),
                _ => unreachable!(),
            };
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Texld => {
            let dst = inst.dst.unwrap();
            let sample = texld_sample_expr(
                inst,
                const_defs_f32,
                const_base,
                stage,
                sampler_texture_types,
            );
            emit_assign(wgsl, *indent, dst, &sample);
        }
        Op::Rcp => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("(vec4<f32>(1.0) / {})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Rsq => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("inverseSqrt({})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Exp => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("exp2({})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Log => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("log2({})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Dsx => {
            if stage == ShaderStage::Pixel {
                let dst = inst.dst.unwrap();
                let expr = derivative_expr(inst, const_defs_f32, const_base, Op::Dsx);
                emit_assign(wgsl, *indent, dst, &expr);
            }
        }
        Op::Dsy => {
            if stage == ShaderStage::Pixel {
                let dst = inst.dst.unwrap();
                let expr = derivative_expr(inst, const_defs_f32, const_base, Op::Dsy);
                emit_assign(wgsl, *indent, dst, &expr);
            }
        }
        Op::Pow => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let src1 = src_expr(&inst.src[1], const_defs_f32, const_base);
            let mut expr = format!("pow({}, {})", src0, src1);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::Frc => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("fract({})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            emit_assign(wgsl, *indent, dst, &expr);
        }
        Op::If => {
            let cond = src_expr(&inst.src[0], const_defs_f32, const_base);
            push_indent(wgsl, *indent);
            wgsl.push_str(&format!("if (({}).x != 0.0) {{\n", cond));
            *indent += 1;
        }
        Op::Ifc => {
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let op = match inst.imm.unwrap_or(0) {
                0 => ">",
                1 => "==",
                2 => ">=",
                3 => "<",
                4 => "!=",
                5 => "<=",
                _ => "==",
            };
            push_indent(wgsl, *indent);
            wgsl.push_str(&format!("if (({}).x {} ({}).x) {{\n", a, op, b));
            *indent += 1;
        }
        Op::Else => {
            *indent = indent.saturating_sub(1);
            push_indent(wgsl, *indent);
            wgsl.push_str("} else {\n");
            *indent += 1;
        }
        Op::EndIf => {
            *indent = indent.saturating_sub(1);
            push_indent(wgsl, *indent);
            wgsl.push_str("}\n");
        }
    }
}

fn apply_result_modifier(expr: String, modifier: ResultModifier) -> String {
    let mut out = expr;
    out = match modifier.shift {
        ResultShift::None => out,
        ResultShift::Mul2 => format!("({}) * 2.0", out),
        ResultShift::Mul4 => format!("({}) * 4.0", out),
        ResultShift::Mul8 => format!("({}) * 8.0", out),
        ResultShift::Div2 => format!("({}) / 2.0", out),
        ResultShift::Div4 => format!("({}) / 4.0", out),
        ResultShift::Div8 => format!("({}) / 8.0", out),
        ResultShift::Unknown(_) => out,
    };
    if modifier.saturate {
        out = format!("clamp({}, vec4<f32>(0.0), vec4<f32>(1.0))", out);
    }
    out
}

fn src_expr(src: &Src, const_defs_f32: &BTreeMap<u16, [f32; 4]>, const_base: u32) -> String {
    let base = match src.reg.file {
        RegisterFile::Const => {
            if const_defs_f32.contains_key(&src.reg.index) {
                format!("c{}{}", src.reg.index, swizzle_suffix(src.swizzle))
            } else {
                format!(
                    "constants.c[{}u]{}",
                    u32::from(src.reg.index) + const_base,
                    swizzle_suffix(src.swizzle)
                )
            }
        }
        _ => format!("{}{}", reg_var_name(src.reg), swizzle_suffix(src.swizzle)),
    };

    match src.modifier {
        SrcModifier::None => base,
        SrcModifier::Negate => format!("-({})", base),
        SrcModifier::Bias => format!("(({}) - vec4<f32>(0.5))", base),
        SrcModifier::BiasNegate => format!("-(({}) - vec4<f32>(0.5))", base),
        SrcModifier::Sign => format!("(({}) * 2.0 - vec4<f32>(1.0))", base),
        SrcModifier::SignNegate => format!("-(({}) * 2.0 - vec4<f32>(1.0))", base),
        SrcModifier::Comp => format!("(vec4<f32>(1.0) - ({}))", base),
        SrcModifier::X2 => format!("(({}) * 2.0)", base),
        SrcModifier::X2Negate => format!("-(({}) * 2.0)", base),
        SrcModifier::Dz => format!("(({}) / ({}).z)", base, base),
        SrcModifier::Dw => format!("(({}) / ({}).w)", base, base),
        SrcModifier::Abs => format!("abs({})", base),
        SrcModifier::AbsNegate => format!("-abs({})", base),
        SrcModifier::Not => format!("(vec4<f32>(1.0) - ({}))", base),
    }
}

fn wgsl_f32(v: f32) -> String {
    // WGSL uses abstract numeric literals, but we format floats with an explicit decimal point to
    // keep the generated code unambiguous and stable for tests.
    let mut s = format!("{v:.8}");
    if let Some(dot) = s.find('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.len() == dot + 1 {
            s.push('0');
        }
    }
    s
}

#[derive(Debug, Clone)]
pub struct CachedShader {
    pub hash: Hash,
    pub ir: ShaderIr,
    pub wgsl: WgslOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderCacheLookupSource {
    /// The shader was already present in the in-memory cache.
    Memory,
    /// The translator ran and the output was inserted into the in-memory cache.
    Translated,
}

#[derive(Debug, Clone, Copy)]
pub struct ShaderCacheLookup<'a> {
    pub source: ShaderCacheLookupSource,
    shader: &'a CachedShader,
}

impl std::ops::Deref for ShaderCacheLookup<'_> {
    type Target = CachedShader;

    fn deref(&self) -> &Self::Target {
        self.shader
    }
}

pub struct ShaderCache {
    map: HashMap<Hash, CachedShader>,
    wgsl_options: WgslOptions,
}

impl ShaderCache {
    pub fn new(wgsl_options: WgslOptions) -> Self {
        Self {
            map: HashMap::new(),
            wgsl_options,
        }
    }

    pub fn wgsl_options(&self) -> WgslOptions {
        self.wgsl_options
    }

    pub fn set_wgsl_options(&mut self, wgsl_options: WgslOptions) {
        if self.wgsl_options != wgsl_options {
            self.wgsl_options = wgsl_options;
            self.map.clear();
        }
    }

    pub fn get_or_translate(&mut self, bytes: &[u8]) -> Result<ShaderCacheLookup<'_>, ShaderError> {
        use std::collections::hash_map::Entry;

        if bytes.len() > MAX_D3D9_SHADER_BLOB_BYTES {
            return Err(ShaderError::BytecodeTooLarge {
                len: bytes.len(),
                max: MAX_D3D9_SHADER_BLOB_BYTES,
            });
        }

        let hash = blake3::hash(bytes);
        match self.map.entry(hash) {
            Entry::Occupied(e) => Ok(ShaderCacheLookup {
                source: ShaderCacheLookupSource::Memory,
                shader: e.into_mut(),
            }),
            Entry::Vacant(e) => {
                let program = parse(bytes)?;
                let ir = to_ir(&program);
                let wgsl = generate_wgsl_with_options(&ir, self.wgsl_options)?;
                let hash = *e.key();
                Ok(ShaderCacheLookup {
                    source: ShaderCacheLookupSource::Translated,
                    shader: e.insert(CachedShader { hash, ir, wgsl }),
                })
            }
        }
    }
}

impl Default for ShaderCache {
    fn default() -> Self {
        Self::new(WgslOptions::default())
    }
}
