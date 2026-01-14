use crate::shader_limits::{
    MAX_D3D9_COLOR_OUTPUT_REGISTER_INDEX, MAX_D3D9_INPUT_REGISTER_INDEX,
    MAX_D3D9_SAMPLER_REGISTER_INDEX, MAX_D3D9_SHADER_BYTECODE_BYTES,
    MAX_D3D9_SHADER_REGISTER_INDEX, MAX_D3D9_SHADER_TOKEN_COUNT, MAX_D3D9_TEMP_REGISTER_INDEX,
    MAX_D3D9_TEXTURE_REGISTER_INDEX,
};
use crate::sm3::types::{ShaderStage, ShaderVersion};

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
    /// Distance vector helper (`dst`).
    ///
    /// D3D9 `dst` computes a packed helper vector:
    /// - `dst.x = 1.0`
    /// - `dst.y = src0.y * src1.y`
    /// - `dst.z = src0.z`
    /// - `dst.w = src1.w`
    ///
    /// Swizzles and source modifiers are applied to both operands before this packing.
    Dst,
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
    /// Repeat loop (`rep` / `endrep`).
    ///
    /// D3D9 `rep` repeats the enclosed block a fixed number of times (derived from an integer
    /// constant register). The loop counter is stored in the `aL` loop register.
    Rep,
    EndRep,
    Break,
    Breakc,
    Call,
    CallNz,
    Ret,
    Label,
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
            16 => Self::Lit, // 0x10
            17 => Self::Dst, // 0x11
            12 => Self::Slt,
            13 => Self::Sge,
            14 => Self::Exp,
            15 => Self::Log,
            19 => Self::Frc,  // 0x13
            20 => Self::M4x4, // 0x14
            21 => Self::M4x3, // 0x15
            22 => Self::M3x4, // 0x16
            23 => Self::M3x3, // 0x17
            24 => Self::M3x2, // 0x18
            25 => Self::Call,
            26 => Self::CallNz,
            27 => Self::Loop,
            28 => Self::Ret,
            29 => Self::EndLoop,
            30 => Self::Label,
            31 => Self::Dcl,
            32 => Self::Pow,
            33 => Self::Crs,    // 0x21
            34 => Self::Sgn,    // 0x22
            35 => Self::Abs,    // 0x23
            36 => Self::Nrm,    // 0x24
            37 => Self::SinCos, // 0x25
            38 => Self::Rep,
            39 => Self::EndRep,
            40 => Self::If,
            41 => Self::Ifc,
            42 => Self::Else,
            43 => Self::EndIf,
            44 => Self::Break,
            45 => Self::Breakc,
            46 => Self::Mova,
            65 => Self::TexKill, // 0x41
            66 => Self::Tex,     // 0x42 (texld/texldp)
            // Some tooling emits `setp` with opcode 0x4E, while the D3D9 SDK opcode
            // table lists it at 0x5E. Accept both.
            78 => Self::Setp,   // 0x4E
            93 => Self::TexLdd, // 0x5D
            94 => Self::Setp,   // 0x5E
            95 => Self::TexLdl, // 0x5F
            81 => Self::Def,    // 0x51
            82 => Self::DefI,   // 0x52
            83 => Self::DefB,   // 0x53
            // Some opcode tables list `seq`/`sne` (set on equal / set on not equal)
            // in this neighborhood. We accept the commonly cited values; anything
            // else is treated as `Unknown`.
            84 => Self::Seq,    // 0x54
            85 => Self::Sne,    // 0x55
            86 => Self::Dsx,    // 0x56
            87 => Self::Dsy,    // 0x57
            88 => Self::Cmp,    // 0x58
            89 => Self::Dp2Add, // 0x59
            90 => Self::Dp2,    // 0x5A
            0xFFFE => Self::Comment,
            0xFFFF => Self::End,
            other => Self::Unknown(other),
        }
    }

    /// Returns the raw `D3DSHADER_INSTRUCTION_OPCODE_TYPE` value.
    #[deny(unreachable_patterns)]
    pub fn raw(&self) -> u16 {
        match self {
            Self::Nop => 0,
            Self::Mov => 1,
            Self::Mova => 46,
            Self::Add => 2,
            Self::Sub => 3,
            Self::Mad => 4,
            Self::M4x4 => 20,
            Self::M4x3 => 21,
            Self::M3x4 => 22,
            Self::M3x3 => 23,
            Self::M3x2 => 24,
            Self::Lrp => 18,
            Self::Mul => 5,
            Self::Rcp => 6,
            Self::Rsq => 7,
            Self::Dp2Add => 89,
            Self::Dp2 => 90,
            Self::Dp3 => 8,
            Self::Dp4 => 9,
            Self::Exp => 14,
            Self::Log => 15,
            Self::Lit => 16,
            Self::Dst => 17,
            Self::Min => 10,
            Self::Max => 11,
            Self::Abs => 35,
            Self::Crs => 33,
            Self::Sgn => 34,
            Self::Slt => 12,
            Self::Sge => 13,
            Self::Frc => 19,
            Self::Nrm => 36,
            Self::SinCos => 37,
            Self::Call => 25,
            Self::CallNz => 26,
            Self::Loop => 27,
            Self::Ret => 28,
            Self::EndLoop => 29,
            Self::Label => 30,
            Self::Dcl => 31,
            Self::Pow => 32,
            Self::Rep => 38,
            Self::EndRep => 39,
            Self::If => 40,
            Self::Ifc => 41,
            Self::Else => 42,
            Self::EndIf => 43,
            Self::Break => 44,
            Self::Breakc => 45,
            Self::TexKill => 65,
            Self::Tex => 66,
            Self::TexLdd => 93,
            Self::Setp => 94,
            Self::TexLdl => 95,
            Self::Def => 81,
            Self::DefI => 82,
            Self::DefB => 83,
            Self::Seq => 84,
            Self::Sne => 85,
            Self::Dsx => 86,
            Self::Dsy => 87,
            Self::Cmp => 88,
            Self::Comment => 0xFFFE,
            Self::End => 0xFFFF,
            Self::Unknown(raw) => *raw,
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
            Self::Dst => "dst",
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
            Self::Rep => "rep",
            Self::EndRep => "endrep",
            Self::Break => "break",
            Self::Breakc => "breakc",
            Self::Call => "call",
            Self::CallNz => "callnz",
            Self::Ret => "ret",
            Self::Label => "label",
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
    let supported_version = match major {
        2 => minor <= 1,
        3 => minor == 0,
        _ => false,
    };
    if !supported_version {
        return Err(DecodeError {
            token_index: 0,
            message: format!("unsupported shader model {major}.{minor}"),
        });
    }

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

        // D3D9 shader instruction length is encoded in bits 24..27.
        //
        // Different toolchains appear to disagree on whether this value includes the opcode token
        // itself. In the wild we see both:
        //   - total instruction length in DWORDs including the opcode token (e.g. DXBC SHDR/SHEX)
        //   - parameter-token count excluding the opcode token (some legacy token streams)
        //
        // For robustness, try both encodings.
        let length_field = ((opcode_token >> 24) & 0x0F) as usize;

        let coissue = (opcode_token & COISSUE) != 0;
        let predicated = (opcode_token & PREDICATED) != 0;
        let result_modifier = decode_result_modifier(opcode_token);

        let length_including_opcode = if length_field == 0 { 1 } else { length_field };
        let length_excluding_opcode = 1 + length_field;
        let mut length_candidates = [length_including_opcode, length_excluding_opcode];
        // Keep output deterministic and avoid trying the same length twice when length_field==0.
        if length_candidates[0] == length_candidates[1] {
            length_candidates[1] = 0;
        }

        let decode_predicate = |pred_token: u32,
                                abs_token_index: usize|
         -> Result<Predicate, DecodeError> {
            let (pred_src, consumed) =
                decode_src_operand(&[pred_token], 0, stage, major).map_err(|mut err| {
                    err.token_index += abs_token_index;
                    err
                })?;
            if consumed != 1 {
                return Err(DecodeError {
                    token_index: abs_token_index,
                    message: "unexpected multi-token predicate operand".to_owned(),
                });
            }
            if pred_src.reg.file != RegisterFile::Predicate {
                return Err(DecodeError {
                    token_index: abs_token_index,
                    message: format!("expected predicate register, got {:?}", pred_src.reg.file),
                });
            }

            let (component, negate) = match pred_src.modifier {
                SrcModifier::None => (pred_src.swizzle.0[0], false),
                SrcModifier::Negate => (pred_src.swizzle.0[0], true),
                other => {
                    return Err(DecodeError {
                        token_index: abs_token_index,
                        message: format!("unsupported predicate modifier {other:?}"),
                    });
                }
            };

            Ok(Predicate {
                reg: pred_src.reg,
                component,
                negate,
            })
        };

        // When we try multiple interpretations of the opcode length field, prefer the
        // error from the first attempt (the "length includes opcode" encoding) if
        // *all* attempts fail. This keeps error messages stable and typically
        // points at the true failure rather than a secondary parse error from an
        // alternative length interpretation.
        let mut first_err: Option<DecodeError> = None;
        let mut last_err: Option<DecodeError> = None;
        let mut decoded = None;

        for length in length_candidates.into_iter().filter(|&l| l != 0) {
            if token_index + length > tokens.len() {
                let err = DecodeError {
                    token_index,
                    message: format!(
                        "instruction length {length} exceeds remaining tokens {}",
                        tokens.len() - token_index
                    ),
                };
                if first_err.is_none() {
                    first_err = Some(err.clone());
                }
                last_err = Some(err);
                break;
            }

            let operand_tokens_full = &tokens[token_index + 1..token_index + length];

            let pred_positions: &[(bool, usize)] = if predicated {
                // Some streams encode the predicate token as a prefix, others as a suffix.
                // Try both; the predicate token must decode to a predicate register for either to
                // be accepted.
                &[
                    // (is_prefix, operand_base_offset)
                    (true, 1),
                    (false, 0),
                ]
            } else {
                &[(false, 0)]
            };

            for &(pred_is_prefix, operand_base_offset) in pred_positions {
                let (predicate, operand_tokens) = if predicated {
                    if operand_tokens_full.is_empty() {
                        let err = DecodeError {
                            token_index,
                            message: "predicated instruction missing predicate token".to_owned(),
                        };
                        if first_err.is_none() {
                            first_err = Some(err.clone());
                        }
                        last_err = Some(err);
                        continue;
                    }
                    if pred_is_prefix {
                        let pred_abs = token_index + 1;
                        let pred = match decode_predicate(operand_tokens_full[0], pred_abs) {
                            Ok(p) => p,
                            Err(err) => {
                                if first_err.is_none() {
                                    first_err = Some(err.clone());
                                }
                                last_err = Some(err);
                                continue;
                            }
                        };
                        (Some(pred), &operand_tokens_full[1..])
                    } else {
                        let pred_abs = token_index + length - 1;
                        let pred = match decode_predicate(
                            *operand_tokens_full.last().unwrap(),
                            pred_abs,
                        ) {
                            Ok(p) => p,
                            Err(err) => {
                                if first_err.is_none() {
                                    first_err = Some(err.clone());
                                }
                                last_err = Some(err);
                                continue;
                            }
                        };
                        (
                            Some(pred),
                            &operand_tokens_full[..operand_tokens_full.len() - 1],
                        )
                    }
                } else {
                    (None, operand_tokens_full)
                };

                match decode_operands_and_extras(opcode_token, opcode, stage, major, operand_tokens)
                {
                    Ok((operands, dcl, comment_data)) => {
                        decoded = Some((length, predicate, operands, dcl, comment_data));
                        break;
                    }
                    Err(mut err) => {
                        // Convert from operand-token-local index to absolute token index.
                        err.token_index += location.token_index + 1 + operand_base_offset;
                        if first_err.is_none() {
                            first_err = Some(err.clone());
                        }
                        last_err = Some(err);
                    }
                }
            }

            if decoded.is_some() {
                break;
            }
        }

        let Some((length, predicate, operands, dcl, comment_data)) = decoded else {
            return Err(
                first_err
                    .or(last_err)
                    .unwrap_or_else(|| DecodeError {
                    token_index,
                    message: "failed to decode instruction".to_owned(),
                }),
            );
        };

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
        | Opcode::EndRep
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
                    message: format!("opcode {} is only valid in pixel shaders", opcode.name()),
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
            let (src0, src0_consumed) =
                decode_src_operand(operand_tokens, dst_consumed, stage, major)?;
            operands.push(Operand::Src(src0));
            let mut token_cursor = dst_consumed + src0_consumed;
            if token_cursor == operand_tokens.len() {
                // 2-operand form: dst, src0
            } else {
                // 4-operand form: dst, src0, src1, src2
                let (src1, src1_consumed) =
                    decode_src_operand(operand_tokens, token_cursor, stage, major)?;
                operands.push(Operand::Src(src1));
                token_cursor += src1_consumed;
                let (src2, src2_consumed) =
                    decode_src_operand(operand_tokens, token_cursor, stage, major)?;
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
        | Opcode::Dst
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
        Opcode::Rep => {
            // SM2/3 `rep` takes 1 operand: an integer constant register that specifies the repeat
            // count (in `.x`).
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Src],
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
        Opcode::CallNz => {
            // callnz: label, cond
            parse_fixed_operands(
                opcode,
                stage,
                major,
                operand_tokens,
                &[OperandKind::Src, OperandKind::Src],
                &mut operands,
            )?;
        }
        Opcode::Label => {
            // label: label
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
            // D3D9 `dcl` encoding differs between toolchains:
            //
            // - Modern SM2/SM3 compilers encode `dcl <dst>` with the declaration metadata packed
            //   into opcode_token[16..24] (usage / usage_index, or texture type for samplers).
            //
            // - Older assemblers encode `dcl <decl_token>, <dst>` where the decl token contains
            //   usage/usage_index (or sampler texture type in decl_token[27..31]).
            //
            // Accept both forms so we can decode real `fxc`-produced shaders and synthetic tests.
            let mut decl_token: Option<u32> = None;
            match decode_dst_operand(operand_tokens, 0, stage, major) {
                Ok((dst, consumed)) if consumed == operand_tokens.len() => {
                    operands.push(Operand::Dst(dst));
                }
                _ => {
                    if operand_tokens.len() < 2 {
                        return Err(DecodeError {
                            token_index: 0,
                            message: "dcl missing destination operand".to_owned(),
                        });
                    }
                    decl_token = Some(operand_tokens[0]);
                    let (dst, consumed) = decode_dst_operand(operand_tokens, 1, stage, major)?;
                    if 1 + consumed != operand_tokens.len() {
                        return Err(DecodeError {
                            token_index: 0,
                            message: format!(
                                "opcode {} has extra trailing tokens after dcl destination operand",
                                opcode.name()
                            ),
                        });
                    }
                    operands.push(Operand::Dst(dst));
                }
            }

            let first_operand = operands.first();
            if let Some(decl_token) = decl_token {
                // Legacy form: first operand token is a decl token, followed by a dst operand.
                let dst_is_sampler = matches!(
                    first_operand,
                    Some(Operand::Dst(dst)) if dst.reg.file == RegisterFile::Sampler
                );
                let usage_raw = if dst_is_sampler {
                    ((decl_token >> 27) & 0xF) as u8
                } else {
                    (decl_token & 0x1F) as u8
                };
                let usage_index = ((decl_token >> 16) & 0xF) as u8;

                let usage = decode_dcl_usage(usage_raw, first_operand)?;
                dcl = Some(DclInfo { usage, usage_index });
            } else {
                // Modern form: declaration metadata is packed into opcode_token[16..24].
                //
                // Note: Pixel shader SM2 `dcl t#` is commonly emitted with these bits all zeroed,
                // so do not trust the opcode token's usage fields for texture register declarations.
                // However, SM3 pixel shaders use `dcl_* v#` for varyings and do encode semantics in
                // the opcode token, so accept it for `v#` input decls (and for samplers).
                let is_sampler_decl = matches!(
                    first_operand,
                    Some(Operand::Dst(dst)) if dst.reg.file == RegisterFile::Sampler
                );
                let is_input_decl = matches!(
                    first_operand,
                    Some(Operand::Dst(dst)) if dst.reg.file == RegisterFile::Input
                );
                if stage == ShaderStage::Vertex || is_sampler_decl || is_input_decl {
                    let usage_raw = ((opcode_token >> 16) & 0xF) as u8;
                    let usage_index = ((opcode_token >> 20) & 0xF) as u8;
                    let usage = decode_dcl_usage(usage_raw, first_operand)?;
                    dcl = Some(DclInfo { usage, usage_index });
                } else {
                    dcl = Some(DclInfo {
                        usage: DclUsage::Unknown(0xFF),
                        usage_index: 0,
                    });
                }
            }
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
            // `tex` (opcode 66 / 0x42) uses an opcode-specific "specific" field in bits 16..19
            // to select between variants:
            //   0 = texld
            //   1 = texldp
            //   2 = texldb
            //
            // Preserve this as an immediate so the IR builder doesn't need to peek at opcode token
            // bits.
            let specific = (opcode_token >> 16) & 0xF;
            operands.push(Operand::Imm32(specific));
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
        Opcode::Unknown(_op) => {
            // Keep unknown opcodes as-is so tooling can still report coverage over
            // partially-supported shader corpora.
            //
            // We don't know the operand encoding, so preserve the raw tokens as
            // `imm32` operands.
            operands.extend(operand_tokens.iter().copied().map(Operand::Imm32));
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
            message: format!("register index {index} in {file:?} exceeds maximum {max_index}"),
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
        if matches!(texture_type, TextureType::Unknown(_)) {
            return Err(DecodeError {
                token_index: 0,
                message: format!("unsupported sampler texture type {texture_type:?}"),
            });
        }
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
