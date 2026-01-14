use crate::sm3::types::{ShaderStage, ShaderVersion};
use crate::shader_limits::{
    MAX_D3D9_COLOR_OUTPUT_REGISTER_INDEX, MAX_D3D9_INPUT_REGISTER_INDEX,
    MAX_D3D9_SAMPLER_REGISTER_INDEX, MAX_D3D9_SHADER_BYTECODE_BYTES, MAX_D3D9_SHADER_REGISTER_INDEX,
    MAX_D3D9_SHADER_TOKEN_COUNT, MAX_D3D9_TEMP_REGISTER_INDEX, MAX_D3D9_TEXTURE_REGISTER_INDEX,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    pub token_index: usize,
    pub message: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DX9 shader decode error at token {}: {}",
            self.token_index, self.message
        )
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionLocation {
    pub instruction_index: usize,
    pub token_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedShader {
    pub version: ShaderVersion,
    pub instructions: Vec<DecodedInstruction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedInstruction {
    pub location: InstructionLocation,
    pub opcode: Opcode,
    /// Total length of the instruction in tokens, including the opcode token.
    pub length: u8,
    pub coissue: bool,
    pub result_modifier: ResultModifier,
    pub predicate: Option<Predicate>,
    pub operands: Vec<Operand>,
    pub dcl: Option<DclInfo>,
    pub comment_data: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Opcode {
    Nop,
    Mov,
    Mova,
    Add,
    Sub,
    Mad,
    Lrp,
    Mul,
    /// D3D9 `dp2add`: 2-component dot product plus add (`dot(src0.xy, src1.xy) + src2.x`),
    /// replicated to all components.
    Dp2Add,
    /// D3D9 `dp2`: 2-component dot product (`dot(src0.xy, src1.xy)`), replicated to all components.
    Dp2,
    Dp3,
    Dp4,
    Exp,
    Log,
    M4x4,
    M4x3,
    M3x4,
    M3x3,
    M3x2,
    Rcp,
    Rsq,
    Frc,
    Min,
    Max,
    Abs,
    /// Cross product (`dst.xyz = cross(src0.xyz, src1.xyz)`).
    Crs,
    /// Sign (`dst = sign(src)` component-wise).
    Sgn,
    Nrm,
    Lit,
    SinCos,
    Sge,
    Slt,
    Seq,
    Sne,
    /// D3D9 `dsx` (aka `ddx`): screen-space derivative w.r.t. x, pixel shaders only.
    Dsx,
    /// D3D9 `dsy` (aka `ddy`): screen-space derivative w.r.t. y, pixel shaders only.
    Dsy,
    /// D3D9 `cmp`: per-component select `dst = (src0 >= 0) ? src1 : src2`.
    Cmp,
    If,
    Ifc,
    Else,
    EndIf,
    Loop,
    EndLoop,
    Break,
    Breakc,
    Call,
    Ret,
    Dcl,
    Def,
    DefI,
    DefB,
    Setp,
    Tex,
    TexKill,
    TexLdd,
    TexLdl,
    Pow,
    Comment,
    End,
    Unknown(u16),
}

impl Opcode {
    pub fn from_raw(op: u16) -> Self {
        match op {
            0 => Self::Nop,
            1 => Self::Mov,
            2 => Self::Add,
            3 => Self::Sub,
            4 => Self::Mad,
            5 => Self::Mul,
            18 => Self::Lrp,
            6 => Self::Rcp,
            7 => Self::Rsq,
            8 => Self::Dp3,
            9 => Self::Dp4,
            10 => Self::Min,
            11 => Self::Max,
            16 => Self::Lit,    // 0x10
            12 => Self::Slt,
            13 => Self::Sge,
            14 => Self::Exp,
            15 => Self::Log,
            19 => Self::Frc, // 0x13
            20 => Self::M4x4, // 0x14
            21 => Self::M4x3, // 0x15
            22 => Self::M3x4, // 0x16
            23 => Self::M3x3, // 0x17
            24 => Self::M3x2, // 0x18
            25 => Self::Call,
            27 => Self::Loop,
            28 => Self::Ret,
            29 => Self::EndLoop,
            31 => Self::Dcl,
            32 => Self::Pow,
            33 => Self::Crs, // 0x21
            34 => Self::Sgn, // 0x22
            35 => Self::Abs,   // 0x23
            36 => Self::Nrm,    // 0x24
            37 => Self::SinCos, // 0x25
            40 => Self::If,
            41 => Self::Ifc,
            42 => Self::Else,
            43 => Self::EndIf,
            44 => Self::Break,
            45 => Self::Breakc,
            46 => Self::Mova,
            65 => Self::TexKill, // 0x41
            66 => Self::Tex,     // 0x42 (texld/texldp)
            77 => Self::TexLdd,  // 0x4D
            78 => Self::Setp,    // 0x4E
            79 => Self::TexLdl,  // 0x4F
            81 => Self::Def,     // 0x51
            82 => Self::DefI,    // 0x52
            83 => Self::DefB,    // 0x53
            // Some opcode tables list `seq`/`sne` (set on equal / set on not equal)
            // in this neighborhood. We accept the commonly cited values; anything
            // else is treated as `Unknown`.
            84 => Self::Seq, // 0x54
            85 => Self::Sne, // 0x55
            86 => Self::Dsx, // 0x56
            87 => Self::Dsy, // 0x57
            88 => Self::Cmp, // 0x58
            89 => Self::Dp2Add, // 0x59
            90 => Self::Dp2, // 0x5A
            0xFFFE => Self::Comment,
            0xFFFF => Self::End,
            other => Self::Unknown(other),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Nop => "nop",
            Self::Mov => "mov",
            Self::Mova => "mova",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mad => "mad",
            Self::Lrp => "lrp",
            Self::Mul => "mul",
            Self::Dp2Add => "dp2add",
            Self::Dp2 => "dp2",
            Self::Dp3 => "dp3",
            Self::Dp4 => "dp4",
            Self::Exp => "exp",
            Self::Log => "log",
            Self::M4x4 => "m4x4",
            Self::M4x3 => "m4x3",
            Self::M3x4 => "m3x4",
            Self::M3x3 => "m3x3",
            Self::M3x2 => "m3x2",
            Self::Rcp => "rcp",
            Self::Rsq => "rsq",
            Self::Frc => "frc",
            Self::Min => "min",
            Self::Max => "max",
            Self::Abs => "abs",
            Self::Crs => "crs",
            Self::Sgn => "sgn",
            Self::Nrm => "nrm",
            Self::Lit => "lit",
            Self::SinCos => "sincos",
            Self::Sge => "sge",
            Self::Slt => "slt",
            Self::Seq => "seq",
            Self::Sne => "sne",
            Self::Dsx => "dsx",
            Self::Dsy => "dsy",
            Self::Cmp => "cmp",
            Self::If => "if",
            Self::Ifc => "ifc",
            Self::Else => "else",
            Self::EndIf => "endif",
            Self::Loop => "loop",
            Self::EndLoop => "endloop",
            Self::Break => "break",
            Self::Breakc => "breakc",
            Self::Call => "call",
            Self::Ret => "ret",
            Self::Dcl => "dcl",
            Self::Def => "def",
            Self::DefI => "defi",
            Self::DefB => "defb",
            Self::Setp => "setp",
            Self::Tex => "tex",
            Self::TexKill => "texkill",
            Self::TexLdd => "texldd",
            Self::TexLdl => "texldl",
            Self::Pow => "pow",
            Self::Comment => "comment",
            Self::End => "end",
            Self::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandKind {
    Dst,
    Src,
    Imm32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Dst(DstOperand),
    Src(SrcOperand),
    Imm32(u32),
}

impl Operand {
    pub fn kind(&self) -> OperandKind {
        match self {
            Operand::Dst(_) => OperandKind::Dst,
            Operand::Src(_) => OperandKind::Src,
            Operand::Imm32(_) => OperandKind::Imm32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    pub reg: RegisterRef,
    pub component: SwizzleComponent,
    pub negate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstOperand {
    pub reg: RegisterRef,
    pub mask: WriteMask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrcOperand {
    pub reg: RegisterRef,
    pub swizzle: Swizzle,
    pub modifier: SrcModifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRef {
    pub file: RegisterFile,
    pub index: u32,
    pub relative: Option<Box<RelativeAddress>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeAddress {
    pub reg: Box<RegisterRef>,
    pub component: SwizzleComponent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegisterFile {
    Temp,
    Input,
    Const,
    Addr,
    Texture,
    RastOut,
    AttrOut,
    TexCoordOut,
    Output,
    ConstInt,
    ColorOut,
    DepthOut,
    Sampler,
    ConstBool,
    Loop,
    Label,
    Predicate,
    MiscType,
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegDecodeContext {
    Src,
    Dst,
    Relative,
}

impl RegisterFile {
    fn from_raw(raw: u8, stage: ShaderStage, major: u8, ctx: RegDecodeContext) -> Self {
        // Register type values follow `D3DSHADER_PARAM_REGISTER_TYPE` from the
        // Direct3D 9 SDK. Some encodings are stage-dependent:
        //   - type 3 is `a#` (vertex) or `t#` (pixel)
        //   - type 8 is `o#` (vertex) or `oC#` (pixel)
        //
        // Additionally, type 3 used in a relative-addressing token (the token
        // after a parameter with the RELATIVE bit set) is always treated as an
        // address register, regardless of stage.
        match raw {
            0 => Self::Temp,
            1 => Self::Input,
            2 => Self::Const,
            3 => match ctx {
                RegDecodeContext::Relative => Self::Addr,
                RegDecodeContext::Src | RegDecodeContext::Dst => match stage {
                    ShaderStage::Vertex => Self::Addr,
                    ShaderStage::Pixel => Self::Texture,
                },
            },
            4 => Self::RastOut,
            5 => Self::AttrOut,
            6 => {
                if stage == ShaderStage::Vertex && major >= 3 {
                    // VS 3.0 uses the generic `o#` output register file, which shares
                    // the same underlying register type encoding as legacy `oT#`.
                    Self::Output
                } else {
                    Self::TexCoordOut
                }
            }
            7 => Self::ConstInt,
            8 => match stage {
                ShaderStage::Vertex => Self::Output,
                ShaderStage::Pixel => Self::ColorOut,
            },
            9 => Self::DepthOut,
            10 => Self::Sampler,
            11..=13 => Self::Const,
            14 => Self::ConstBool,
            15 => Self::Loop,
            17 => Self::MiscType,
            18 => Self::Label,
            19 => Self::Predicate,
            other => Self::Unknown(other),
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Temp => "r",
            Self::Input => "v",
            Self::Const => "c",
            Self::Addr => "a",
            Self::Texture => "t",
            Self::RastOut => "oPos",
            Self::AttrOut => "oD",
            Self::TexCoordOut => "oT",
            Self::Output => "o",
            Self::ConstInt => "i",
            Self::ColorOut => "oC",
            Self::DepthOut => "oDepth",
            Self::Sampler => "s",
            Self::ConstBool => "b",
            Self::Loop => "aL",
            Self::Label => "l",
            Self::Predicate => "p",
            Self::MiscType => "misc",
            Self::Unknown(_) => "?",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteMask(pub u8);

impl WriteMask {
    pub fn all() -> Self {
        Self(0xF)
    }

    pub fn contains(&self, component: SwizzleComponent) -> bool {
        let bit = match component {
            SwizzleComponent::X => 0,
            SwizzleComponent::Y => 1,
            SwizzleComponent::Z => 2,
            SwizzleComponent::W => 3,
        };
        (self.0 & (1 << bit)) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swizzle(pub [SwizzleComponent; 4]);

impl Swizzle {
    pub fn identity() -> Self {
        Self([
            SwizzleComponent::X,
            SwizzleComponent::Y,
            SwizzleComponent::Z,
            SwizzleComponent::W,
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwizzleComponent {
    X,
    Y,
    Z,
    W,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    Unknown(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DclInfo {
    pub usage: DclUsage,
    pub usage_index: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DclUsage {
    Position,
    BlendWeight,
    BlendIndices,
    Normal,
    PointSize,
    TexCoord,
    Tangent,
    Binormal,
    TessFactor,
    PositionT,
    Color,
    Fog,
    Depth,
    Sample,
    TextureType(TextureType),
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureType {
    Texture2D,
    TextureCube,
    Texture3D,
    Texture1D,
    Unknown(u8),
}

pub fn decode_u8_le_bytes(bytes: &[u8]) -> Result<DecodedShader, DecodeError> {
    if bytes.len() > MAX_D3D9_SHADER_BYTECODE_BYTES {
        return Err(DecodeError {
            token_index: 0,
            message: format!(
                "bytecode length {} exceeds maximum {} bytes",
                bytes.len(),
                MAX_D3D9_SHADER_BYTECODE_BYTES
            ),
        });
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(DecodeError {
            token_index: 0,
            message: format!("bytecode length {} is not a multiple of 4", bytes.len()),
        });
    }
    let token_count = bytes.len() / 4;
    if token_count > MAX_D3D9_SHADER_TOKEN_COUNT {
        return Err(DecodeError {
            token_index: 0,
            message: format!(
                "token count {token_count} exceeds maximum {MAX_D3D9_SHADER_TOKEN_COUNT}"
            ),
        });
    }
    let mut tokens = Vec::with_capacity(token_count);
    for chunk in bytes.chunks_exact(4) {
        let token = u32::from_le_bytes(chunk.try_into().unwrap());
        tokens.push(token);
    }
    decode_u32_tokens(&tokens)
}

pub fn decode_u32_tokens(tokens: &[u32]) -> Result<DecodedShader, DecodeError> {
    if tokens.len() > MAX_D3D9_SHADER_TOKEN_COUNT {
        return Err(DecodeError {
            token_index: 0,
            message: format!(
                "token count {} exceeds maximum {}",
                tokens.len(),
                MAX_D3D9_SHADER_TOKEN_COUNT
            ),
        });
    }
    if tokens.is_empty() {
        return Err(DecodeError {
            token_index: 0,
            message: "empty token stream".to_owned(),
        });
    }

    let version_token = tokens[0];
    let (stage, major, minor) = decode_version_token(version_token).ok_or_else(|| DecodeError {
        token_index: 0,
        message: format!("unknown shader version token 0x{version_token:08x}"),
    })?;

    let version = ShaderVersion {
        stage,
        major,
        minor,
    };

    let mut instructions = Vec::new();
    let mut token_index = 1usize;
    let mut instruction_index = 0usize;

    while token_index < tokens.len() {
        let opcode_token = tokens[token_index];
        let opcode_raw = (opcode_token & OPCODE_MASK) as u16;
        let opcode = Opcode::from_raw(opcode_raw);

        let location = InstructionLocation {
            instruction_index,
            token_index,
        };
        instruction_index += 1;

        if opcode == Opcode::Comment {
            let comment_len = ((opcode_token >> 16) & 0x7FFF) as usize;
            let total_len = 1usize.checked_add(comment_len).ok_or_else(|| DecodeError {
                token_index,
                message: "comment length overflow".to_owned(),
            })?;
            if token_index + total_len > tokens.len() {
                return Err(DecodeError {
                    token_index,
                    message: format!(
                        "comment length {comment_len} exceeds remaining tokens {}",
                        tokens.len() - token_index
                    ),
                });
            }

            let comment_data = tokens[token_index + 1..token_index + total_len].to_vec();
            instructions.push(DecodedInstruction {
                location,
                opcode,
                length: total_len as u8,
                coissue: false,
                result_modifier: ResultModifier::default(),
                predicate: None,
                operands: Vec::new(),
                dcl: None,
                comment_data: Some(comment_data),
            });
            token_index += total_len;
            continue;
        }

        if opcode == Opcode::End {
            instructions.push(DecodedInstruction {
                location,
                opcode,
                length: 1,
                coissue: false,
                result_modifier: ResultModifier::default(),
                predicate: None,
                operands: Vec::new(),
                dcl: None,
                comment_data: None,
            });
            break;
        }

        // D3D9 SM2/SM3 encodes instruction length as the total number of DWORD tokens in the
        // instruction, including the opcode token itself, in bits 24..27.
        //
        // Many instructions that take no operands encode this length as `0`, which is interpreted
        // as a 1-token instruction.
        let mut length = ((opcode_token >> 24) & 0x0F) as usize;
        if length == 0 {
            length = 1;
        }
        if token_index + length > tokens.len() {
            return Err(DecodeError {
                token_index,
                message: format!(
                    "instruction length {length} exceeds remaining tokens {}",
                    tokens.len() - token_index
                ),
            });
        }

        let coissue = (opcode_token & COISSUE) != 0;
        let predicated = (opcode_token & PREDICATED) != 0;
        let result_modifier = decode_result_modifier(opcode_token);

        let mut operand_tokens = &tokens[token_index + 1..token_index + length];

        let predicate = if predicated {
            // In SM3 bytecode, predicated instructions append a predicate register
            // source parameter token at the end of the instruction.
            //
            // Predicate registers do not support relative addressing, so a single
            // token is expected here.
            if operand_tokens.is_empty() {
                return Err(DecodeError {
                    token_index,
                    message: "predicated instruction missing predicate token".to_owned(),
                });
            }
            let pred_token = *operand_tokens.last().unwrap();
            operand_tokens = &operand_tokens[..operand_tokens.len() - 1];
            let (pred_src, consumed) = decode_src_operand(&[pred_token], 0, stage, major)?;
            if consumed != 1 {
                return Err(DecodeError {
                    token_index,
                    message: "unexpected multi-token predicate operand".to_owned(),
                });
            }
            let (component, negate) = match pred_src.modifier {
                SrcModifier::None => (pred_src.swizzle.0[0], false),
                SrcModifier::Negate => (pred_src.swizzle.0[0], true),
                other => {
                    return Err(DecodeError {
                        token_index,
                        message: format!("unsupported predicate modifier {other:?}"),
                    });
                }
            };
            Some(Predicate {
                reg: pred_src.reg,
                component,
                negate,
            })
        } else {
            None
        };

        let (operands, dcl, comment_data) =
            decode_operands_and_extras(opcode_token, opcode, stage, major, operand_tokens)
                .map_err(|mut err| {
                    err.token_index += location.token_index + 1;
                    err
                })?;

        instructions.push(DecodedInstruction {
            location,
            opcode,
            length: length as u8,
            coissue,
            result_modifier,
            predicate,
            operands,
            dcl,
            comment_data,
        });

        token_index += length;
    }

    // D3D9 SM2/SM3 token streams are terminated by an `end` instruction (opcode 0xFFFF). Treat
    // missing termination as malformed input: without an explicit end token, callers may
    // accidentally accept truncated streams (e.g. empty shaders that only contain the version
    // token) and downstream code may assume termination semantics that aren't satisfied.
    if !matches!(instructions.last().map(|i| i.opcode), Some(Opcode::End)) {
        return Err(DecodeError {
            token_index: tokens.len().saturating_sub(1),
            message: "missing end token".to_owned(),
        });
    }

    Ok(DecodedShader {
        version,
        instructions,
    })
}

type OperandsAndExtras = (Vec<Operand>, Option<DclInfo>, Option<Vec<u32>>);

fn decode_operands_and_extras(
    opcode_token: u32,
    opcode: Opcode,
    stage: ShaderStage,
    major: u8,
    operand_tokens: &[u32],
) -> Result<OperandsAndExtras, DecodeError> {
    let mut operands = Vec::new();
    let mut dcl = None;
    let comment_data = None;

    match opcode {
        Opcode::Nop
        | Opcode::Else
        | Opcode::EndIf
        | Opcode::EndLoop
        | Opcode::Break
        | Opcode::Ret => {
            if !operand_tokens.is_empty() {
                return Err(DecodeError {
                    token_index: 0,
                    message: format!(
                        "opcode {} expected no operands but has {} tokens",
                        opcode.name(),
                        operand_tokens.len()
                    ),
                });
            }
        }
        Opcode::Mov
        | Opcode::Rcp
        | Opcode::Rsq
        | Opcode::Frc
        | Opcode::Exp
        | Opcode::Log
        | Opcode::Abs
        | Opcode::Sgn
        | Opcode::Nrm
        | Opcode::Lit => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Dst, OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::Mova => {
            // mova: dst, src
            //
            // Address registers share the same underlying register type encoding as `t#` (type 3)
            // in pixel shaders. When used as the destination of `mova`, type 3 must be interpreted
            // as an address register (`a#`) even for pixel shaders.
            let (dst, dst_consumed) = decode_dst_operand_mova(operand_tokens, 0, stage, major)?;
            operands.push(Operand::Dst(dst));
            let (src, src_consumed) =
                decode_src_operand(operand_tokens, dst_consumed, stage, major)?;
            operands.push(Operand::Src(src));
            if dst_consumed + src_consumed != operand_tokens.len() {
                return Err(DecodeError {
                    token_index: dst_consumed + src_consumed,
                    message: format!(
                        "opcode {} decoded {} operand tokens but instruction has {}",
                        opcode.name(),
                        dst_consumed + src_consumed,
                        operand_tokens.len()
                    ),
                });
            }
        }
        Opcode::Dsx | Opcode::Dsy => {
            if stage != ShaderStage::Pixel {
                return Err(DecodeError {
                    token_index: 0,
                    message: format!(
                        "opcode {} is only valid in pixel shaders",
                        opcode.name()
                    ),
                });
            }
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Dst, OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::SinCos => {
            // SM3 `sincos` has multiple encodings / operand counts. We currently
            // support the common forms:
            //   - sincos dst, src0
            //   - sincos dst, src0, src1, src2
            //
            // Other forms are rejected for now.
            //
            // Bits 16..19 are opcode-specific; reject any non-zero "specific"
            // field since we don't yet model those variants.
            let specific = (opcode_token >> 16) & 0xF;
            if specific != 0 {
                return Err(DecodeError {
                    token_index: 0,
                    message: format!(
                        "opcode {} has unsupported encoding (specific=0x{specific:x})",
                        opcode.name()
                    ),
                });
            }

            // The number of *operands* is variable, but the number of *tokens* per operand is also
            // variable (e.g. relative addressing consumes an additional token). Decode sequentially
            // instead of branching on `operand_tokens.len()`.
            let (dst, dst_consumed) = decode_dst_operand(operand_tokens, 0, stage, major)?;
            operands.push(Operand::Dst(dst));
            let (src0, src0_consumed) = decode_src_operand(operand_tokens, dst_consumed, stage, major)?;
            operands.push(Operand::Src(src0));
            let mut token_cursor = dst_consumed + src0_consumed;
            if token_cursor == operand_tokens.len() {
                // 2-operand form: dst, src0
            } else {
                // 4-operand form: dst, src0, src1, src2
                let (src1, src1_consumed) = decode_src_operand(operand_tokens, token_cursor, stage, major)?;
                operands.push(Operand::Src(src1));
                token_cursor += src1_consumed;
                let (src2, src2_consumed) = decode_src_operand(operand_tokens, token_cursor, stage, major)?;
                operands.push(Operand::Src(src2));
                token_cursor += src2_consumed;

                if token_cursor != operand_tokens.len() {
                    return Err(DecodeError {
                        token_index: token_cursor,
                        message: format!(
                            "opcode {} decoded {} operand tokens but instruction has {}",
                            opcode.name(),
                            token_cursor,
                            operand_tokens.len()
                        ),
                    });
                }
            }
        }
        Opcode::Add
        | Opcode::Sub
        | Opcode::Mul
        | Opcode::Min
        | Opcode::Max
        | Opcode::Sge
        | Opcode::Slt
        | Opcode::Seq
        | Opcode::Sne
        | Opcode::Dp2
        | Opcode::Dp3
        | Opcode::Dp4
        | Opcode::Crs
        | Opcode::Pow
        | Opcode::M4x4
        | Opcode::M4x3
        | Opcode::M3x4
        | Opcode::M3x3
        | Opcode::M3x2 => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Dst, OperandKind::Src, OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::Dp2Add => {
            // dp2add dst, src0, src1, src2
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[
                    OperandKind::Dst,
                    OperandKind::Src,
                    OperandKind::Src,
                    OperandKind::Src,
                ],
                &mut operands,
            )?;
        }
        Opcode::Mad => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[
                    OperandKind::Dst,
                    OperandKind::Src,
                    OperandKind::Src,
                    OperandKind::Src,
                ],
                &mut operands,
            )?;
        }
        Opcode::Cmp => {
            // D3D9 `cmp`: dst, src0, src1, src2.
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[
                    OperandKind::Dst,
                    OperandKind::Src,
                    OperandKind::Src,
                    OperandKind::Src,
                ],
                &mut operands,
            )?;
        }
        Opcode::Lrp => {
            // lrp dst, src0, src1, src2
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[
                    OperandKind::Dst,
                    OperandKind::Src,
                    OperandKind::Src,
                    OperandKind::Src,
                ],
                &mut operands,
            )?;
        }
        Opcode::If => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::Ifc | Opcode::Breakc => {
            // Comparison type is encoded in opcode_token[16..20].
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Src, OperandKind::Src],
                &mut operands,
            )?;

            // Store compare info via an extra synthetic operand, to keep the decoder
            // output fully self-contained and avoid leaking token-level details into
            // the IR builder.
            //
            // Operand layout:
            //   - src0
            //   - src1
            //   - imm32(compare_op)
            let cmp = (opcode_token >> 16) & 0x7;
            operands.push(Operand::Imm32(cmp));
        }
        Opcode::Loop => {
            // SM2/3 `loop` takes 2 operands: loop register and integer constant.
            // We keep them as generic src operands (the loop register is a register
            // file, not a write).
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Src, OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::Call => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::Dcl => {
            // SM2/3 `dcl` is a single-parameter instruction in the executable stream:
            //   dcl <dst_register>
            //
            // The declaration metadata (usage / texture type) is encoded in opcode_token[16..24],
            // matching `D3DSHADER_DECL_USAGE` / `D3DSHADER_SAMPLER_TEXTURE_TYPE`.
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Dst],
                &mut operands,
            )?;
            let usage_raw = ((opcode_token >> 16) & 0xF) as u8;
            let usage_index = ((opcode_token >> 20) & 0xF) as u8;
            let usage = decode_dcl_usage(usage_raw, operands.first())?;
            dcl = Some(DclInfo { usage, usage_index });
        }
        Opcode::Def => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[
                    OperandKind::Dst,
                    OperandKind::Imm32,
                    OperandKind::Imm32,
                    OperandKind::Imm32,
                    OperandKind::Imm32,
                ],
                &mut operands,
            )?;
        }
        Opcode::DefI => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[
                    OperandKind::Dst,
                    OperandKind::Imm32,
                    OperandKind::Imm32,
                    OperandKind::Imm32,
                    OperandKind::Imm32,
                ],
                &mut operands,
            )?;
        }
        Opcode::DefB => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Dst, OperandKind::Imm32],
                &mut operands,
            )?;
        }
        Opcode::Setp => {
            // Comparison type is encoded in opcode_token[16..20].
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Dst, OperandKind::Src, OperandKind::Src],
                &mut operands,
            )?;
            let cmp = (opcode_token >> 16) & 0x7;
            operands.push(Operand::Imm32(cmp));
        }
        Opcode::Tex => {
            // ps_2_0/3_0 texld: dst, coord, sampler
            // Old ps_1_x tex has different signature; we use token count heuristics.
            if operand_tokens.len() >= 3 {
                parse_fixed_operands(
                    opcode,
                    stage,
                    major,
                    operand_tokens,
                    &[OperandKind::Dst, OperandKind::Src, OperandKind::Src],
                    &mut operands,
                )?;
            } else if operand_tokens.len() == 2 {
                parse_fixed_operands(
                    opcode,
                    stage,
                    major,
                    operand_tokens,
                    &[OperandKind::Dst, OperandKind::Src],
                    &mut operands,
                )?;
            } else {
                return Err(DecodeError {
                    token_index: 0,
                    message: format!(
                        "opcode {} expected >=2 operand tokens but has {}",
                        opcode.name(),
                        operand_tokens.len()
                    ),
                });
            }
            // texldp is encoded by a flag in opcode_token[16].
            let project = (opcode_token >> 16) & 0x1;
            operands.push(Operand::Imm32(project));
        }
        Opcode::TexLdd => {
            // texldd: dst, coord, ddx, ddy, sampler
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[
                    OperandKind::Dst,
                    OperandKind::Src,
                    OperandKind::Src,
                    OperandKind::Src,
                    OperandKind::Src,
                ],
                &mut operands,
            )?;
        }
        Opcode::TexLdl => {
            // texldl: dst, coord, sampler
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Dst, OperandKind::Src, OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::TexKill => {
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::Unknown(op) => {
            return Err(DecodeError {
                token_index: 0,
                message: format!("unsupported opcode 0x{op:04x}"),
            });
        }
        Opcode::Comment | Opcode::End => unreachable!("handled in main loop"),
    }

    Ok((operands, dcl, comment_data))
}

fn parse_fixed_operands(
    opcode: Opcode,
    stage: ShaderStage,
    major: u8,
    operand_tokens: &[u32],
    pattern: &[OperandKind],
    out: &mut Vec<Operand>,
) -> Result<(), DecodeError> {
    let mut token_cursor = 0usize;
    for expected in pattern {
        match expected {
            OperandKind::Dst => {
                let (dst, consumed) =
                    decode_dst_operand(operand_tokens, token_cursor, stage, major)?;
                out.push(Operand::Dst(dst));
                token_cursor += consumed;
            }
            OperandKind::Src => {
                let (src, consumed) =
                    decode_src_operand(operand_tokens, token_cursor, stage, major)?;
                out.push(Operand::Src(src));
                token_cursor += consumed;
            }
            OperandKind::Imm32 => {
                let token = operand_tokens
                    .get(token_cursor)
                    .ok_or_else(|| DecodeError {
                        token_index: token_cursor,
                        message: format!("opcode {} missing immediate operand", opcode.name()),
                    })?;
                out.push(Operand::Imm32(*token));
                token_cursor += 1;
            }
        }
    }

    if token_cursor != operand_tokens.len() {
        return Err(DecodeError {
            token_index: token_cursor,
            message: format!(
                "opcode {} decoded {} operand tokens but instruction has {}",
                opcode.name(),
                token_cursor,
                operand_tokens.len()
            ),
        });
    }
    Ok(())
}

fn decode_version_token(token: u32) -> Option<(ShaderStage, u8, u8)> {
    let high = token & 0xFFFF_0000;
    let stage = match high {
        0xFFFE_0000 => ShaderStage::Vertex,
        0xFFFF_0000 => ShaderStage::Pixel,
        _ => return None,
    };
    let major = ((token >> 8) & 0xFF) as u8;
    let minor = (token & 0xFF) as u8;
    Some((stage, major, minor))
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

const OPCODE_MASK: u32 = 0x0000_FFFF;
const COISSUE: u32 = 0x4000_0000;
const PREDICATED: u32 = 0x1000_0000;

const REGNUM_MASK: u32 = 0x0000_07FF;
const REGTYPE_MASK: u32 = 0x7000_0000;
const REGTYPE_SHIFT: u32 = 28;
const REGTYPE_MASK2: u32 = 0x0000_1800;
const REGTYPE_SHIFT2: u32 = 8;
const RELATIVE: u32 = 0x0000_2000;

const WRITEMASK_MASK: u32 = 0x000F_0000;
const WRITEMASK_SHIFT: u32 = 16;

const SWIZZLE_MASK: u32 = 0x00FF_0000;
const SWIZZLE_SHIFT: u32 = 16;

const SRCMOD_MASK: u32 = 0x0F00_0000;
const SRCMOD_SHIFT: u32 = 24;

fn decode_dst_operand(
    tokens: &[u32],
    start: usize,
    stage: ShaderStage,
    major: u8,
) -> Result<(DstOperand, usize), DecodeError> {
    let token = *tokens.get(start).ok_or_else(|| DecodeError {
        token_index: start,
        message: "unexpected end of operand tokens".to_owned(),
    })?;

    let (reg, reg_consumed) =
        decode_register_ref(tokens, start, stage, major, RegDecodeContext::Dst)?;
    let mut mask = ((token & WRITEMASK_MASK) >> WRITEMASK_SHIFT) as u8;
    if mask == 0 {
        mask = 0xF;
    }
    Ok((
        DstOperand {
            reg,
            mask: WriteMask(mask),
        },
        reg_consumed,
    ))
}

fn decode_dst_operand_mova(
    tokens: &[u32],
    start: usize,
    stage: ShaderStage,
    major: u8,
) -> Result<(DstOperand, usize), DecodeError> {
    let token = *tokens.get(start).ok_or_else(|| DecodeError {
        token_index: start,
        message: "unexpected end of operand tokens".to_owned(),
    })?;

    // Decode the destination register as an address register even for pixel shaders.
    // Using `RegDecodeContext::Relative` forces register type 3 -> `Addr` regardless of stage and
    // also rejects invalid nested relative addressing on the destination operand.
    let (reg, reg_consumed) =
        decode_register_ref(tokens, start, stage, major, RegDecodeContext::Relative)?;

    let mut mask = ((token & WRITEMASK_MASK) >> WRITEMASK_SHIFT) as u8;
    if mask == 0 {
        mask = 0xF;
    }
    Ok((
        DstOperand {
            reg,
            mask: WriteMask(mask),
        },
        reg_consumed,
    ))
}

fn decode_src_operand(
    tokens: &[u32],
    start: usize,
    stage: ShaderStage,
    major: u8,
) -> Result<(SrcOperand, usize), DecodeError> {
    let token = *tokens.get(start).ok_or_else(|| DecodeError {
        token_index: start,
        message: "unexpected end of operand tokens".to_owned(),
    })?;

    let (reg, reg_consumed) =
        decode_register_ref(tokens, start, stage, major, RegDecodeContext::Src)?;
    let swizzle_raw = ((token & SWIZZLE_MASK) >> SWIZZLE_SHIFT) as u8;
    let swizzle = decode_swizzle(swizzle_raw);
    let modifier_raw = ((token & SRCMOD_MASK) >> SRCMOD_SHIFT) as u8;
    let modifier = decode_src_modifier(modifier_raw);

    Ok((
        SrcOperand {
            reg,
            swizzle,
            modifier,
        },
        reg_consumed,
    ))
}

fn decode_register_ref(
    tokens: &[u32],
    start: usize,
    stage: ShaderStage,
    major: u8,
    ctx: RegDecodeContext,
) -> Result<(RegisterRef, usize), DecodeError> {
    let token = *tokens.get(start).ok_or_else(|| DecodeError {
        token_index: start,
        message: "unexpected end of operand tokens".to_owned(),
    })?;

    let index = token & REGNUM_MASK;
    let regtype_raw = (((token & REGTYPE_MASK) >> REGTYPE_SHIFT)
        | ((token & REGTYPE_MASK2) >> REGTYPE_SHIFT2)) as u8;
    let file = RegisterFile::from_raw(regtype_raw, stage, major, ctx);
    let max_index = match file {
        RegisterFile::Temp => MAX_D3D9_TEMP_REGISTER_INDEX,
        RegisterFile::Input => MAX_D3D9_INPUT_REGISTER_INDEX,
        RegisterFile::Const => MAX_D3D9_SHADER_REGISTER_INDEX,
        RegisterFile::Texture => MAX_D3D9_TEXTURE_REGISTER_INDEX,
        RegisterFile::Sampler => MAX_D3D9_SAMPLER_REGISTER_INDEX,
        RegisterFile::ColorOut => MAX_D3D9_COLOR_OUTPUT_REGISTER_INDEX,
        // Most other register files are either special (single-register) or are not yet used by
        // Aero's SM3-to-WGSL lowering. Keep a conservative cap to avoid rejecting otherwise-valid
        // shaders while still preventing pathological indices.
        _ => MAX_D3D9_SHADER_REGISTER_INDEX,
    };
    if index > max_index {
        return Err(DecodeError {
            token_index: start,
            message: format!(
                "register index {index} in {file:?} exceeds maximum {max_index}"
            ),
        });
    }
    let mut consumed = 1usize;

    let relative = if (token & RELATIVE) != 0 {
        if ctx == RegDecodeContext::Relative {
            return Err(DecodeError {
                token_index: start,
                message: "nested relative addressing not supported".to_owned(),
            });
        }
        let relative_token_index = start + 1;
        let rel_token = *tokens
            .get(relative_token_index)
            .ok_or_else(|| DecodeError {
                token_index: relative_token_index,
                message: "relative addressing missing register token".to_owned(),
            })?;
        let (rel_reg, rel_consumed) = decode_register_ref(
            tokens,
            relative_token_index,
            stage,
            major,
            RegDecodeContext::Relative,
        )?;
        if rel_consumed != 1 {
            return Err(DecodeError {
                token_index: relative_token_index,
                message: "nested relative addressing not supported".to_owned(),
            });
        }
        let swizzle_raw = ((rel_token & SWIZZLE_MASK) >> SWIZZLE_SHIFT) as u8;
        let rel_swizzle = decode_swizzle(swizzle_raw);
        consumed += rel_consumed;
        Some(Box::new(RelativeAddress {
            reg: Box::new(rel_reg),
            component: rel_swizzle.0[0],
        }))
    } else {
        None
    };

    Ok((
        RegisterRef {
            file,
            index,
            relative,
        },
        consumed,
    ))
}

fn decode_swizzle(swizzle: u8) -> Swizzle {
    let mut comps = [SwizzleComponent::X; 4];
    for (i, comp) in comps.iter_mut().enumerate() {
        let bits = (swizzle >> (i * 2)) & 0x3;
        *comp = match bits {
            0 => SwizzleComponent::X,
            1 => SwizzleComponent::Y,
            2 => SwizzleComponent::Z,
            _ => SwizzleComponent::W,
        };
    }
    Swizzle(comps)
}

fn decode_src_modifier(modifier: u8) -> SrcModifier {
    match modifier {
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
        other => SrcModifier::Unknown(other),
    }
}

fn decode_dcl_usage(
    usage_raw: u8,
    first_operand: Option<&Operand>,
) -> Result<DclUsage, DecodeError> {
    let is_sampler_decl = match first_operand {
        Some(Operand::Dst(dst)) => dst.reg.file == RegisterFile::Sampler,
        _ => false,
    };

    if is_sampler_decl {
        let texture_type = match usage_raw {
            0 => TextureType::Unknown(0),
            1 => TextureType::Texture1D,
            2 => TextureType::Texture2D,
            3 => TextureType::TextureCube,
            4 => TextureType::Texture3D,
            other => TextureType::Unknown(other),
        };
        return Ok(DclUsage::TextureType(texture_type));
    }

    Ok(match usage_raw {
        0 => DclUsage::Position,
        1 => DclUsage::BlendWeight,
        2 => DclUsage::BlendIndices,
        3 => DclUsage::Normal,
        4 => DclUsage::PointSize,
        5 => DclUsage::TexCoord,
        6 => DclUsage::Tangent,
        7 => DclUsage::Binormal,
        8 => DclUsage::TessFactor,
        9 => DclUsage::PositionT,
        10 => DclUsage::Color,
        11 => DclUsage::Fog,
        12 => DclUsage::Depth,
        13 => DclUsage::Sample,
        other => DclUsage::Unknown(other),
    })
}
