use crate::token::{decode_dst_param, decode_src_param};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Pixel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderModel {
    pub major: u8,
    pub minor: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterType {
    Temp,
    Input,
    Const,
    Addr,
    RastOut,
    AttrOut,
    TexCoordOutOrOutput,
    ConstInt,
    ColorOut,
    DepthOut,
    Sampler,
    Const2,
    Const3,
    Const4,
    ConstBool,
    Loop,
    TempFloat16,
    MiscType,
    Label,
    Predicate,
    Unknown(u8),
}

impl RegisterType {
    pub fn from_raw(raw: u8) -> Self {
        match raw {
            0 => RegisterType::Temp,
            1 => RegisterType::Input,
            2 => RegisterType::Const,
            3 => RegisterType::Addr,
            4 => RegisterType::RastOut,
            5 => RegisterType::AttrOut,
            6 => RegisterType::TexCoordOutOrOutput,
            7 => RegisterType::ConstInt,
            8 => RegisterType::ColorOut,
            9 => RegisterType::DepthOut,
            10 => RegisterType::Sampler,
            11 => RegisterType::Const2,
            12 => RegisterType::Const3,
            13 => RegisterType::Const4,
            14 => RegisterType::ConstBool,
            15 => RegisterType::Loop,
            16 => RegisterType::TempFloat16,
            17 => RegisterType::MiscType,
            18 => RegisterType::Label,
            19 => RegisterType::Predicate,
            other => RegisterType::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Register {
    pub ty: RegisterType,
    pub num: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    Position,
    Normal,
    TexCoord,
    Color,
    Fog,
    Depth,
    Sample,
    Unknown(u8),
}

impl Usage {
    pub fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Usage::Position,
            3 => Usage::Normal,
            5 => Usage::TexCoord,
            10 => Usage::Color,
            11 => Usage::Fog,
            12 => Usage::Depth,
            13 => Usage::Sample,
            other => Usage::Unknown(other),
        }
    }

    pub fn mnemonic(self) -> &'static str {
        match self {
            Usage::Position => "position",
            Usage::Normal => "normal",
            Usage::TexCoord => "texcoord",
            Usage::Color => "color",
            Usage::Fog => "fog",
            Usage::Depth => "depth",
            Usage::Sample => "sample",
            Usage::Unknown(_) => "usage",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplerTextureType {
    Texture2D,
    Cube,
    Volume,
    Unknown(u8),
}

impl SamplerTextureType {
    pub fn from_raw(raw: u8) -> Self {
        match raw {
            2 => SamplerTextureType::Texture2D,
            3 => SamplerTextureType::Cube,
            4 => SamplerTextureType::Volume,
            other => SamplerTextureType::Unknown(other),
        }
    }

    pub fn mnemonic(self) -> &'static str {
        match self {
            SamplerTextureType::Texture2D => "2d",
            SamplerTextureType::Cube => "cube",
            SamplerTextureType::Volume => "volume",
            SamplerTextureType::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Dcl {
        reg: Register,
        usage: Usage,
        usage_index: u8,
    },
    Sampler {
        reg: Register,
        texture_type: SamplerTextureType,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swizzle {
    pub x: SwizzleComp,
    pub y: SwizzleComp,
    pub z: SwizzleComp,
    pub w: SwizzleComp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwizzleComp {
    X,
    Y,
    Z,
    W,
}

impl SwizzleComp {
    fn from_raw(raw: u8) -> Self {
        match raw & 0x3 {
            0 => SwizzleComp::X,
            1 => SwizzleComp::Y,
            2 => SwizzleComp::Z,
            _ => SwizzleComp::W,
        }
    }

    pub fn as_char(self) -> char {
        match self {
            SwizzleComp::X => 'x',
            SwizzleComp::Y => 'y',
            SwizzleComp::Z => 'z',
            SwizzleComp::W => 'w',
        }
    }
}

impl Swizzle {
    pub fn identity() -> Self {
        Self {
            x: SwizzleComp::X,
            y: SwizzleComp::Y,
            z: SwizzleComp::Z,
            w: SwizzleComp::W,
        }
    }

    pub fn from_byte(swz: u8) -> Self {
        Self {
            x: SwizzleComp::from_raw(swz),
            y: SwizzleComp::from_raw(swz >> 2),
            z: SwizzleComp::from_raw(swz >> 4),
            w: SwizzleComp::from_raw(swz >> 6),
        }
    }

    pub fn is_identity(self) -> bool {
        self == Swizzle::identity()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcModifier {
    None,
    Neg,
    Abs,
    AbsNeg,
    Unknown(u8),
}

impl SrcModifier {
    pub fn from_raw(raw: u8) -> Self {
        match raw {
            0 => SrcModifier::None,
            1 => SrcModifier::Neg,
            11 => SrcModifier::Abs,
            12 => SrcModifier::AbsNeg,
            other => SrcModifier::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativeAddress {
    pub reg: Register,
    pub component: SwizzleComp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstParam {
    pub reg: Register,
    pub write_mask: u8,
    pub saturate: bool,
    pub partial_precision: bool,
    pub centroid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrcParam {
    Register {
        reg: Register,
        swizzle: Swizzle,
        modifier: SrcModifier,
        relative: Option<RelativeAddress>,
    },
    Immediate(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBlock {
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderStats {
    pub max_temp: Option<u16>,
}

impl ShaderStats {
    pub(crate) fn observe_register(&mut self, reg: Register) {
        if reg.ty == RegisterType::Temp {
            self.max_temp = Some(self.max_temp.unwrap_or(0).max(reg.num));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    Op {
        opcode: crate::Opcode,
        predicated: bool,
        coissue: bool,
        predicate: Option<SrcParam>,
        dst: Option<DstParam>,
        src: Vec<SrcParam>,
    },
    Unknown {
        opcode_raw: u16,
        tokens: Vec<u32>,
    },
}

impl Instruction {
    pub(crate) fn observe_stats(&self, stats: &mut ShaderStats) {
        match self {
            Instruction::Op {
                predicate,
                dst,
                src,
                ..
            } => {
                if let Some(dst) = dst {
                    stats.observe_register(dst.reg);
                }
                if let Some(pred) = predicate {
                    observe_src(stats, pred);
                }
                for s in src {
                    observe_src(stats, s);
                }
            }
            Instruction::Unknown { .. } => {}
        }
    }
}

fn observe_src(stats: &mut ShaderStats, src: &SrcParam) {
    if let SrcParam::Register { reg, relative, .. } = src {
        stats.observe_register(*reg);
        if let Some(rel) = relative {
            stats.observe_register(rel.reg);
        }
    }
}

pub(crate) fn decode_dst(token: u32) -> DstParam {
    decode_dst_param(token)
}

pub(crate) fn decode_src(tokens: &[u32], idx: &mut usize) -> SrcParam {
    decode_src_param(tokens, idx)
}
