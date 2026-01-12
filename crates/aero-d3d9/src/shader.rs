//! Shader parsing and translation (DXBC/D3D9 bytecode → IR → WGSL).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use blake3::Hash;
use thiserror::Error;

use crate::dxbc;
use crate::vertex::{DeclUsage, LocationMapError, StandardLocationMap, VertexLocationMap};

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
    Dp3,
    Dp4,
    Rcp,
    Rsq,
    Min,
    Max,
    Cmp,
    Slt,
    Sge,
    Seq,
    Sne,
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
    pub used_samplers: BTreeSet<u16>,
    pub used_consts: BTreeSet<u16>,
    pub used_inputs: BTreeSet<u16>,
    pub used_outputs: BTreeSet<Register>,
    pub temp_count: u16,
    /// True when vertex shader input registers were remapped from raw `v#` indices to canonical
    /// WGSL `@location(n)` values based on `dcl_*` semantics.
    ///
    /// When this is true, the host-side D3D9 executor must bind vertex attributes using the same
    /// semantic-based mapping (see [`StandardLocationMap`]).
    pub uses_semantic_locations: bool,
}

#[derive(Debug, Error)]
pub enum ShaderError {
    #[error("dxbc error: {0}")]
    Dxbc(#[from] dxbc::DxbcError),
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
        3 => RegisterFile::Texture, // also Addr in vs; we treat as texture for pixel shader inputs.
        10 => RegisterFile::Sampler,
        other => return Err(ShaderError::UnsupportedRegisterType(other)),
    };

    Ok(Src {
        reg: Register {
            file,
            index: reg_num,
        },
        swizzle: Swizzle::from_d3d_byte(swizzle_byte),
        modifier,
    })
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
        0x0013 => Some(Op::Frc),
        0x0028 => Some(Op::If),
        0x0029 => Some(Op::Ifc),
        0x002A => Some(Op::Else),
        0x002B => Some(Op::EndIf),
        0x0042 => Some(Op::Texld), // D3DSIO_TEX
        0x0054 => Some(Op::Seq),
        0x0055 => Some(Op::Sne),
        0x0058 => Some(Op::Cmp),
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
    if !(major == 2 || major == 3) {
        return Err(ShaderError::UnsupportedVersion(version_token));
    }
    let version = ShaderVersion {
        stage,
        model: ShaderModel { major, minor },
    };

    let mut idx = 1usize;
    let mut instructions = Vec::new();
    let mut const_defs_f32 = BTreeMap::<u16, [f32; 4]>::new();
    let mut used_samplers = BTreeSet::new();
    let mut used_consts = BTreeSet::new();
    let mut used_inputs = BTreeSet::new();
    let mut used_outputs = BTreeSet::new();
    let mut temp_max = 0u16;
    let mut if_stack = Vec::<bool>::new(); // tracks whether an `else` has been seen for each active `if`
    let mut input_dcl_map = HashMap::<u16, (DeclUsage, u8)>::new();

    while idx < words.len() {
        let token = read_u32(&words, &mut idx)?;
        let opcode = (token & 0xFFFF) as u16;
        // D3D9 instruction length is encoded in bits 24..27 (4 bits). Higher bits in the same
        // byte are flags (predication/co-issue) and must not affect operand count.
        let param_count = ((token >> 24) & 0x0F) as usize;
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

        // Declarations (DCL).
        //
        // Layout (SM2/SM3):
        //   dcl <decl_token>, <dst_register_token>
        //
        // We currently only use vertex shader input declarations to remap D3D9 input registers
        // (`v#`) to canonical WGSL `@location(n)` values.
        if opcode == 0x001F {
            if idx + param_count > words.len() {
                return Err(ShaderError::UnexpectedEof);
            }
            let mut params = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                params.push(read_u32(&words, &mut idx)?);
            }
            if params.len() < 2 {
                return Err(ShaderError::UnexpectedEof);
            }
            let decl_token = params[0];
            let dst_token = params[1];

            // D3D9 `DCL` encoding:
            // - usage_raw = decl_token & 0x1F
            // - usage_index = (decl_token >> 16) & 0xF
            let usage_raw = (decl_token & 0x1F) as u8;
            let usage_index = ((decl_token >> 16) & 0xF) as u8;
            let Ok(usage) = DeclUsage::from_u8(usage_raw) else {
                continue;
            };
            let Ok(dst) = decode_dst(dst_token) else {
                continue;
            };
            if dst.reg.file == RegisterFile::Input {
                input_dcl_map.insert(dst.reg.index, (usage, usage_index));
            }
            continue;
        }

        let mut params = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            params.push(read_u32(&words, &mut idx)?);
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
            let mut vals = [0f32; 4];
            for i in 0..4 {
                vals[i] = f32::from_bits(params[1 + i]);
            }
            const_defs_f32.insert(reg_num, vals);
            continue;
        }

        // The WGSL backend only implements a subset of SM2/SM3. Treat unknown opcodes as no-ops
        // so we can still translate simple shaders while incrementally adding support.
        let Some(op) = opcode_to_op(opcode) else {
            continue;
        };
        if op == Op::End {
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
                if_stack.push(false);
                Instruction {
                    op,
                    dst: None,
                    src: vec![decode_src(params[0])?],
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
                if_stack.push(false);
                Instruction {
                    op,
                    dst: None,
                    src: vec![decode_src(params[0])?, decode_src(params[1])?],
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
            | Op::Dp3
            | Op::Dp4
            | Op::Rcp
            | Op::Rsq
            | Op::Min
            | Op::Max
            | Op::Cmp
            | Op::Slt
            | Op::Sge
            | Op::Seq
            | Op::Sne
            | Op::Frc => {
                if params.len() < 2 {
                    return Err(ShaderError::UnexpectedEof);
                }
                let dst = decode_dst(params[0])?;
                let src = params[1..]
                    .iter()
                    .map(|t| decode_src(*t))
                    .collect::<Result<Vec<_>, _>>()?;
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
                let coord = decode_src(params[1])?;
                let sampler_src = decode_src(params[2])?;
                let sampler_index = if sampler_src.reg.file == RegisterFile::Sampler {
                    sampler_src.reg.index
                } else {
                    return Err(ShaderError::UnsupportedRegisterType(99));
                };
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

    // Apply semantic-based vertex input remapping.
    //
    // D3D9's `dcl_*` declarations associate semantics with input registers (`v#`). The DX9 UMD is
    // free to assign semantics to arbitrary input registers (e.g. COLOR0 might be declared as
    // `v7`). Our WGSL backend uses `@location(v#)` for vertex inputs, so we remap those `v#`
    // indices to a canonical semantic-based location assignment. This makes vertex input binding
    // stable and ensures non-trivial register assignments receive the correct data.
    let mut uses_semantic_locations = false;
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
            let map = StandardLocationMap;
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
        used_samplers,
        used_consts,
        used_inputs,
        used_outputs,
        temp_count: temp_max.max(1),
        uses_semantic_locations,
    })
}

/// Parse DXBC or raw D3D9 shader bytecode into a [`ShaderProgram`].
pub fn parse(bytes: &[u8]) -> Result<ShaderProgram, ShaderError> {
    let token_stream = dxbc::extract_shader_bytecode(bytes)?;
    parse_token_stream(token_stream)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderIr {
    pub version: ShaderVersion,
    pub temp_count: u16,
    pub ops: Vec<Instruction>,
    pub const_defs_f32: BTreeMap<u16, [f32; 4]>,
    pub used_samplers: BTreeSet<u16>,
    pub used_consts: BTreeSet<u16>,
    pub used_inputs: BTreeSet<u16>,
    pub used_outputs: BTreeSet<Register>,
    pub uses_semantic_locations: bool,
}

pub fn to_ir(program: &ShaderProgram) -> ShaderIr {
    ShaderIr {
        version: program.version,
        temp_count: program.temp_count,
        ops: program.instructions.clone(),
        const_defs_f32: program.const_defs_f32.clone(),
        used_samplers: program.used_samplers.clone(),
        used_consts: program.used_consts.clone(),
        used_inputs: program.used_inputs.clone(),
        used_outputs: program.used_outputs.clone(),
        uses_semantic_locations: program.uses_semantic_locations,
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

fn mask_suffix(mask: WriteMask) -> Option<String> {
    if mask == WriteMask::XYZW {
        return None;
    }
    let mut s = String::new();
    if mask.write_x() {
        s.push('x');
    }
    if mask.write_y() {
        s.push('y');
    }
    if mask.write_z() {
        s.push('z');
    }
    if mask.write_w() {
        s.push('w');
    }
    Some(format!(".{}", s))
}

#[derive(Debug, Clone)]
pub struct WgslOutput {
    pub wgsl: String,
    pub entry_point: &'static str,
    pub bind_group_layout: BindGroupLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindGroupLayout {
    /// Binding 0 is always the constants buffer.
    pub sampler_bindings: HashMap<u16, (u32, u32)>, // sampler_index -> (texture_binding, sampler_binding)
}

pub fn generate_wgsl(ir: &ShaderIr) -> WgslOutput {
    let mut wgsl = String::new();

    // Constants: D3D9 has separate `c#` register files for each shader stage. The minimal D3D9
    // token-stream translator models those by packing both stages into a single uniform buffer:
    // - constants.c[0..255]   = vertex shader constants
    // - constants.c[256..511] = pixel shader constants
    wgsl.push_str("struct Constants { c: array<vec4<f32>, 512>, };\n");
    wgsl.push_str("@group(0) @binding(0) var<uniform> constants: Constants;\n\n");

    let mut sampler_bindings = HashMap::new();
    // Allocate bindings: (texture, sampler) pairs.
    //
    // IMPORTANT: Binding numbers must be stable across shader stages. Using a sequential allocator
    // based on "samplers used by this stage" can produce mismatched binding locations when (for
    // example) the vertex shader samples from `s1` while the pixel shader samples from `s0`.
    //
    // Derive binding numbers from the D3D9 sampler register index instead:
    //   texture binding = 1 + 2*s
    //   sampler binding = 2 + 2*s
    for &s in &ir.used_samplers {
        let tex_binding = 1u32 + u32::from(s) * 2;
        let samp_binding = tex_binding + 1;
        sampler_bindings.insert(s, (tex_binding, samp_binding));
        wgsl.push_str(&format!(
            "@group(0) @binding({}) var tex{}: texture_2d<f32>;\n",
            tex_binding, s
        ));
        wgsl.push_str(&format!(
            "@group(0) @binding({}) var samp{}: sampler;\n",
            samp_binding, s
        ));
    }
    if !ir.used_samplers.is_empty() {
        wgsl.push('\n');
    }

    let const_base = match ir.version.stage {
        ShaderStage::Vertex => 0u32,
        ShaderStage::Pixel => 256u32,
    };

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
                );
            }
            debug_assert_eq!(indent, 1, "unbalanced if/endif indentation");

            wgsl.push_str("  var out: VsOutput;\n  out.pos = oPos;\n");
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

            WgslOutput {
                wgsl,
                entry_point: "vs_main",
                bind_group_layout: BindGroupLayout { sampler_bindings },
            }
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
            for inst in &ir.ops {
                emit_inst(
                    &mut wgsl,
                    &mut indent,
                    inst,
                    &ir.const_defs_f32,
                    const_base,
                    ir.version.stage,
                );
            }
            debug_assert_eq!(indent, 1, "unbalanced if/endif indentation");
            wgsl.push_str("  var out: PsOutput;\n");
            for &idx in &color_outputs {
                wgsl.push_str(&format!("  out.oC{} = oC{};\n", idx, idx));
            }
            wgsl.push_str("  return out;\n}\n");

            WgslOutput {
                wgsl,
                entry_point: "fs_main",
                bind_group_layout: BindGroupLayout { sampler_bindings },
            }
        }
    }
}

fn push_indent(wgsl: &mut String, indent: usize) {
    for _ in 0..indent {
        wgsl.push_str("  ");
    }
}

fn emit_inst(
    wgsl: &mut String,
    indent: &mut usize,
    inst: &Instruction,
    const_defs_f32: &BTreeMap<u16, [f32; 4]>,
    const_base: u32,
    stage: ShaderStage,
) {
    match inst.op {
        Op::Nop => {}
        Op::End => {}
        Op::Mov => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let dst_name = reg_var_name(dst.reg);
            let mut expr = src_expr(&src0, const_defs_f32, const_base);
            expr = apply_result_modifier(expr, inst.result_modifier);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Add | Op::Sub | Op::Mul => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let src1 = inst.src[1];
            let dst_name = reg_var_name(dst.reg);
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
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Min | Op::Max => {
            let dst = inst.dst.unwrap();
            let src0 = inst.src[0];
            let src1 = inst.src[1];
            let func = if inst.op == Op::Min { "min" } else { "max" };
            let dst_name = reg_var_name(dst.reg);
            let mut expr = format!(
                "{}({}, {})",
                func,
                src_expr(&src0, const_defs_f32, const_base),
                src_expr(&src1, const_defs_f32, const_base)
            );
            expr = apply_result_modifier(expr, inst.result_modifier);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Mad => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let c = src_expr(&inst.src[2], const_defs_f32, const_base);
            let mut expr = format!("fma({}, {}, {})", a, b, c);
            expr = apply_result_modifier(expr, inst.result_modifier);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Cmp => {
            let dst = inst.dst.unwrap();
            let cond = src_expr(&inst.src[0], const_defs_f32, const_base);
            let a = src_expr(&inst.src[1], const_defs_f32, const_base);
            let b = src_expr(&inst.src[2], const_defs_f32, const_base);
            // Per-component compare: if cond >= 0 then a else b.
            let mut expr = format!("select({}, {}, ({} >= vec4<f32>(0.0)))", b, a, cond);
            expr = apply_result_modifier(expr, inst.result_modifier);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
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
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Dp3 | Op::Dp4 => {
            let dst = inst.dst.unwrap();
            let a = src_expr(&inst.src[0], const_defs_f32, const_base);
            let b = src_expr(&inst.src[1], const_defs_f32, const_base);
            let mut expr = if inst.op == Op::Dp3 {
                format!("vec4<f32>(dot(({}).xyz, ({}).xyz))", a, b)
            } else {
                format!("vec4<f32>(dot({}, {}))", a, b)
            };
            expr = apply_result_modifier(expr, inst.result_modifier);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Texld => {
            let dst = inst.dst.unwrap();
            let coord = inst.src[0];
            let s = inst.sampler.unwrap();
            let dst_name = reg_var_name(dst.reg);
            let coord_expr = src_expr(&coord, const_defs_f32, const_base);
            let project = inst.imm.unwrap_or(0) != 0;
            let uv = if project {
                format!("(({}).xy / ({}).w)", coord_expr, coord_expr)
            } else {
                format!("({}).xy", coord_expr)
            };
            let mut sample = match stage {
                // Vertex stage has no implicit derivatives, so use an explicit LOD.
                ShaderStage::Vertex => {
                    format!("textureSampleLevel(tex{}, samp{}, {}, 0.0)", s, s, uv)
                }
                ShaderStage::Pixel => format!("textureSample(tex{}, samp{}, {})", s, s, uv),
            };
            sample = apply_result_modifier(sample, inst.result_modifier);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, sample, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, sample));
            }
        }
        Op::Rcp => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("(vec4<f32>(1.0) / {})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Rsq => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("inverseSqrt({})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
        }
        Op::Frc => {
            let dst = inst.dst.unwrap();
            let src0 = src_expr(&inst.src[0], const_defs_f32, const_base);
            let mut expr = format!("fract({})", src0);
            expr = apply_result_modifier(expr, inst.result_modifier);
            let dst_name = reg_var_name(dst.reg);
            if let Some(mask) = mask_suffix(dst.mask) {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{}{} = {}{};\n", dst_name, mask, expr, mask));
            } else {
                push_indent(wgsl, *indent);
                wgsl.push_str(&format!("{} = {};\n", dst_name, expr));
            }
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

#[derive(Default)]
pub struct ShaderCache {
    map: HashMap<Hash, CachedShader>,
}

impl ShaderCache {
    pub fn get_or_translate(&mut self, bytes: &[u8]) -> Result<ShaderCacheLookup<'_>, ShaderError> {
        use std::collections::hash_map::Entry;

        let hash = blake3::hash(bytes);
        match self.map.entry(hash) {
            Entry::Occupied(e) => Ok(ShaderCacheLookup {
                source: ShaderCacheLookupSource::Memory,
                shader: e.into_mut(),
            }),
            Entry::Vacant(e) => {
                let program = parse(bytes)?;
                let ir = to_ir(&program);
                let wgsl = generate_wgsl(&ir);
                let hash = *e.key();
                Ok(ShaderCacheLookup {
                    source: ShaderCacheLookupSource::Translated,
                    shader: e.insert(CachedShader { hash, ir, wgsl }),
                })
            }
        }
    }
}
