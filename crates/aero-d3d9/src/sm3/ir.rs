use std::fmt;

use crate::sm3::decode::{
    ResultShift, SrcModifier, Swizzle, SwizzleComponent, TextureType, WriteMask,
};
use crate::sm3::types::ShaderVersion;

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderIr {
    pub version: ShaderVersion,
    pub inputs: Vec<IoDecl>,
    pub outputs: Vec<IoDecl>,
    pub samplers: Vec<SamplerDecl>,
    pub const_defs_f32: Vec<ConstDefF32>,
    /// `defi i#` constants embedded in the shader bytecode.
    pub const_defs_i32: Vec<ConstDefI32>,
    /// `defb b#` constants embedded in the shader bytecode.
    pub const_defs_bool: Vec<ConstDefBool>,
    pub body: Block,
    /// True when vertex shader input registers were remapped from raw `v#` indices to canonical
    /// WGSL `@location(n)` values based on `dcl_*` semantics (see
    /// [`crate::vertex::AdaptiveLocationMap`]).
    pub uses_semantic_locations: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IoDecl {
    pub reg: RegRef,
    pub semantic: Semantic,
    pub mask: WriteMask,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SamplerDecl {
    pub index: u32,
    pub texture_type: TextureType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDefF32 {
    pub index: u32,
    pub value: [f32; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDefI32 {
    pub index: u32,
    pub value: [i32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstDefBool {
    pub index: u32,
    pub value: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

impl Block {
    pub fn new() -> Self {
        Self { stmts: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Op(IrOp),
    If {
        cond: Cond,
        then_block: Block,
        else_block: Option<Block>,
    },
    Loop {
        init: LoopInit,
        body: Block,
    },
    Break,
    BreakIf {
        cond: Cond,
    },
    Discard {
        src: Src,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopInit {
    pub loop_reg: RegRef,
    pub ctrl_reg: RegRef,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrOp {
    Mov {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// Move to address register (`a#`) with float-to-int conversion semantics.
    ///
    /// D3D9 uses a dedicated address register file for relative addressing (e.g.
    /// `cN[a0.x]`). `mova` writes to `a#` registers and converts the source value
    /// to an integer-like representation. The exact rounding behavior is
    /// implementation-defined in this IR and is handled during WGSL lowering.
    Mova {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    Add {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    Sub {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    Mul {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    Mad {
        dst: Dst,
        src0: Src,
        src1: Src,
        src2: Src,
        modifiers: InstModifiers,
    },
    /// Linear interpolation: `dst = src0 * src1 + (1 - src0) * src2` (per-component).
    Lrp {
        dst: Dst,
        src0: Src,
        src1: Src,
        src2: Src,
        modifiers: InstModifiers,
    },
    Dp2 {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    /// `dp2add dst, src0, src1, src2` → `dst = dot(src0.xy, src1.xy) + src2.x` (replicated).
    Dp2Add {
        dst: Dst,
        src0: Src,
        src1: Src,
        src2: Src,
        modifiers: InstModifiers,
    },
    Dp3 {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    Dp4 {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    /// D3D9 matrix multiply helper ops (`m4x4`, `m4x3`, `m3x4`, `m3x3`, `m3x2`, ...).
    ///
    /// `m` is the number of vector components consumed from `src0` (2/3/4) and `n` is the number
    /// of result components written starting at `dst.x`.
    ///
    /// The matrix is sourced from `src1` as `src1 + column_index`, where each register represents
    /// a column vector. This matches typical SM2/3 compiler output where the matrix lives in
    /// consecutive constant registers (`cN..cN+n-1`).
    MatrixMul {
        dst: Dst,
        src0: Src,
        src1: Src,
        m: u8,
        n: u8,
        modifiers: InstModifiers,
    },
    Rcp {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    Rsq {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    Frc {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    Abs {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// Distance vector helper (`dst`).
    ///
    /// `dst.x` is always 1.0, and the remaining components are pairwise products of `src0` and
    /// `src1` (with swizzles/modifiers applied):
    /// `dst = vec4(1, src0.y*src1.y, src0.z*src1.z, src0.w*src1.w)`.
    Dst {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    /// Cross product: `dst.xyz = cross(src0.xyz, src1.xyz)`.
    ///
    /// The W component is not well-specified; we set it to 1.0 for deterministic output.
    Crs {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    /// Component-wise sign: `dst = sign(src)` (−1, 0, +1).
    Sgn {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    Nrm {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// Base-2 exponent: `dst = 2^src`.
    ///
    /// D3D9 SM2/3 `exp` uses base-2 semantics (not natural exponent).
    Exp {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// Base-2 logarithm: `dst = log2(src)`.
    ///
    /// D3D9 SM2/3 `log` uses base-2 semantics (not natural logarithm).
    Log {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// Screen-space derivative w.r.t. X (`dsx` / `ddx`).
    Ddx {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// Screen-space derivative w.r.t. Y (`dsy` / `ddy`).
    Ddy {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// D3D9 `lit` lighting coefficient helper.
    ///
    /// See lowering for the exact component-wise behavior.
    Lit {
        dst: Dst,
        src: Src,
        modifiers: InstModifiers,
    },
    /// D3D9 `sincos` instruction.
    ///
    /// SM2/3 define multiple operand-count variants; the IR represents the
    /// optional extra source operands as `src1`/`src2` when present.
    SinCos {
        dst: Dst,
        src: Src,
        src1: Option<Src>,
        src2: Option<Src>,
        modifiers: InstModifiers,
    },
    Min {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    Max {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    /// Set-on-compare (`sge`, `slt`, `seq`, `sne`, and `setp`).
    SetCmp {
        op: CompareOp,
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    /// D3D9 `cmp`: per-component select `dst = (cond >= 0) ? src_ge : src_lt`.
    Select {
        dst: Dst,
        cond: Src,
        src_ge: Src,
        src_lt: Src,
        modifiers: InstModifiers,
    },
    Pow {
        dst: Dst,
        src0: Src,
        src1: Src,
        modifiers: InstModifiers,
    },
    TexSample {
        kind: TexSampleKind,
        dst: Dst,
        coord: Src,
        ddx: Option<Src>,
        ddy: Option<Src>,
        sampler: u32,
        modifiers: InstModifiers,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexSampleKind {
    ImplicitLod { project: bool },
    /// Texture sampling with an implicit LOD and a bias (`texldb`).
    Bias,
    ExplicitLod,
    Grad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstModifiers {
    pub saturate: bool,
    pub shift: ResultShift,
    pub coissue: bool,
    pub predicate: Option<PredicateRef>,
}

impl InstModifiers {
    pub fn none() -> Self {
        Self {
            saturate: false,
            shift: ResultShift::None,
            coissue: false,
            predicate: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateRef {
    pub reg: RegRef,
    pub component: SwizzleComponent,
    pub negate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dst {
    pub reg: RegRef,
    pub mask: WriteMask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Src {
    pub reg: RegRef,
    pub swizzle: Swizzle,
    pub modifier: SrcModifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegRef {
    pub file: RegFile,
    pub index: u32,
    pub relative: Option<Box<RelativeRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeRef {
    pub reg: Box<RegRef>,
    pub component: SwizzleComponent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegFile {
    Temp,
    Input,
    Const,
    Addr,
    Texture,
    Sampler,
    Predicate,
    RastOut,
    AttrOut,
    TexCoordOut,
    Output,
    ColorOut,
    DepthOut,
    ConstInt,
    ConstBool,
    Loop,
    Label,
    MiscType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Semantic {
    Position(u8),
    BlendWeight(u8),
    BlendIndices(u8),
    Color(u8),
    TexCoord(u8),
    Normal(u8),
    Fog(u8),
    PointSize(u8),
    Depth(u8),
    Tangent(u8),
    Binormal(u8),
    TessFactor(u8),
    PositionT(u8),
    Sample(u8),
    Other { usage: u8, index: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    /// `src.x != 0.0`
    NonZero {
        src: Src,
    },
    Compare {
        op: CompareOp,
        src0: Src,
        src1: Src,
    },
    Predicate {
        pred: PredicateRef,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Gt,
    Ge,
    Eq,
    Ne,
    Lt,
    Le,
    Unknown(u8),
}

impl fmt::Display for ShaderIr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{}_{}_{}",
            stage_prefix(self.version.stage),
            self.version.major,
            self.version.minor
        )?;
        if !self.inputs.is_empty() {
            writeln!(f, "inputs:")?;
            for input in &self.inputs {
                writeln!(
                    f,
                    "  {} = {}",
                    format_reg(&input.reg),
                    format_io_decl(input)
                )?;
            }
        }
        if !self.outputs.is_empty() {
            writeln!(f, "outputs:")?;
            for output in &self.outputs {
                writeln!(
                    f,
                    "  {} = {}",
                    format_reg(&output.reg),
                    format_io_decl(output)
                )?;
            }
        }
        if !self.samplers.is_empty() {
            writeln!(f, "samplers:")?;
            for samp in &self.samplers {
                writeln!(f, "  s{}: {:?}", samp.index, samp.texture_type)?;
            }
        }
        if !self.const_defs_f32.is_empty() {
            writeln!(f, "const-defs:")?;
            for def in &self.const_defs_f32 {
                writeln!(
                    f,
                    "  c{} = [{:.8}, {:.8}, {:.8}, {:.8}]",
                    def.index, def.value[0], def.value[1], def.value[2], def.value[3]
                )?;
            }
        }
        if !self.const_defs_i32.is_empty() {
            writeln!(f, "const-defs-i32:")?;
            for def in &self.const_defs_i32 {
                writeln!(
                    f,
                    "  i{} = [{}, {}, {}, {}]",
                    def.index, def.value[0], def.value[1], def.value[2], def.value[3]
                )?;
            }
        }
        if !self.const_defs_bool.is_empty() {
            writeln!(f, "const-defs-bool:")?;
            for def in &self.const_defs_bool {
                writeln!(f, "  b{} = {}", def.index, def.value)?;
            }
        }
        writeln!(f, "body:")?;
        fmt_block(f, &self.body, 1)
    }
}

fn stage_prefix(stage: crate::sm3::types::ShaderStage) -> &'static str {
    match stage {
        crate::sm3::types::ShaderStage::Vertex => "vs",
        crate::sm3::types::ShaderStage::Pixel => "ps",
    }
}

fn format_reg(reg: &RegRef) -> String {
    let base = match reg.file {
        RegFile::Temp => format!("r{}", reg.index),
        RegFile::Input => format!("v{}", reg.index),
        RegFile::Const => format!("c{}", reg.index),
        RegFile::Addr => format!("a{}", reg.index),
        RegFile::Texture => format!("t{}", reg.index),
        RegFile::Sampler => format!("s{}", reg.index),
        RegFile::Predicate => format!("p{}", reg.index),
        RegFile::RastOut => {
            if reg.index == 0 {
                "oPos".to_owned()
            } else {
                format!("oPos{}", reg.index)
            }
        }
        RegFile::AttrOut => format!("oD{}", reg.index),
        RegFile::TexCoordOut => format!("oT{}", reg.index),
        RegFile::Output => format!("o{}", reg.index),
        RegFile::ColorOut => format!("oC{}", reg.index),
        RegFile::DepthOut => {
            if reg.index == 0 {
                "oDepth".to_owned()
            } else {
                format!("oDepth{}", reg.index)
            }
        }
        RegFile::ConstInt => format!("i{}", reg.index),
        RegFile::ConstBool => format!("b{}", reg.index),
        RegFile::Loop => {
            if reg.index == 0 {
                "aL".to_owned()
            } else {
                format!("aL{}", reg.index)
            }
        }
        RegFile::Label => format!("l{}", reg.index),
        RegFile::MiscType => format!("misc{}", reg.index),
    };
    if let Some(rel) = &reg.relative {
        format!("{}[{}.{:?}]", base, format_reg(&rel.reg), rel.component)
    } else {
        base
    }
}

fn format_io_decl(decl: &IoDecl) -> String {
    let mut s = format!("{:?}", decl.semantic);
    if decl.mask.0 != 0xF {
        s.push_str(&format!(" mask=0x{:x}", decl.mask.0));
    }
    s
}

fn fmt_block(f: &mut fmt::Formatter<'_>, block: &Block, indent: usize) -> fmt::Result {
    for stmt in &block.stmts {
        fmt_stmt(f, stmt, indent)?;
    }
    Ok(())
}

fn fmt_stmt(f: &mut fmt::Formatter<'_>, stmt: &Stmt, indent: usize) -> fmt::Result {
    let pad = "  ".repeat(indent);
    match stmt {
        Stmt::Op(op) => {
            writeln!(f, "{}{}", pad, format_op(op))
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
        } => {
            writeln!(f, "{}if {} {{", pad, format_cond(cond))?;
            fmt_block(f, then_block, indent + 1)?;
            if let Some(else_block) = else_block {
                writeln!(f, "{}}} else {{", pad)?;
                fmt_block(f, else_block, indent + 1)?;
                writeln!(f, "{}}}", pad)
            } else {
                writeln!(f, "{}}}", pad)
            }
        }
        Stmt::Loop { init, body } => {
            writeln!(
                f,
                "{}loop {}, {} {{",
                pad,
                format_reg(&init.loop_reg),
                format_reg(&init.ctrl_reg)
            )?;
            fmt_block(f, body, indent + 1)?;
            writeln!(f, "{}}}", pad)
        }
        Stmt::Break => writeln!(f, "{}break", pad),
        Stmt::BreakIf { cond } => writeln!(f, "{}break_if {}", pad, format_cond(cond)),
        Stmt::Discard { src } => writeln!(f, "{}discard {}", pad, format_src(src)),
    }
}

fn format_op(op: &IrOp) -> String {
    match op {
        IrOp::Mov {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("mov", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Mova {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("mova", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Add {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("add", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Sub {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("sub", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Mul {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("mul", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Mad {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("mad", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone(), src2.clone()])
        ),
        IrOp::Lrp {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("lrp", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone(), src2.clone()])
        ),
        IrOp::Dp2 {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("dp2", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Dp2Add {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("dp2add", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone(), src2.clone()])
        ),
        IrOp::Dp3 {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("dp3", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Dp4 {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("dp4", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::MatrixMul {
            dst,
            src0,
            src1,
            m,
            n,
            modifiers,
        } => {
            let name = format!("m{}x{}", m, n);
            format!(
                "{} {}",
                format_inst(&name, modifiers),
                format_dst_src(dst, &[src0.clone(), src1.clone()])
            )
        }
        IrOp::Rcp {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("rcp", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Rsq {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("rsq", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Frc {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("frc", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Abs {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("abs", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Dst {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("dst", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Crs {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("crs", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Sgn {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("sgn", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Exp {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("exp", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Log {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("log", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Nrm {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("nrm", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Ddx {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("dsx", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Ddy {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("dsy", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::Lit {
            dst,
            src,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("lit", modifiers),
            format_dst_src(dst, std::slice::from_ref(src))
        ),
        IrOp::SinCos {
            dst,
            src,
            src1,
            src2,
            modifiers,
        } => {
            let mut srcs = vec![src.clone()];
            if let Some(src1) = src1 {
                srcs.push(src1.clone());
            }
            if let Some(src2) = src2 {
                srcs.push(src2.clone());
            }
            format!(
                "{} {}",
                format_inst("sincos", modifiers),
                format_dst_src(dst, &srcs)
            )
        }
        IrOp::Min {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("min", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::Max {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("max", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::SetCmp {
            op,
            dst,
            src0,
            src1,
            modifiers,
        } => {
            let name = match op {
                CompareOp::Ge => "sge",
                CompareOp::Lt => "slt",
                CompareOp::Eq => "seq",
                CompareOp::Ne => "sne",
                CompareOp::Gt => "sgt",
                CompareOp::Le => "sle",
                CompareOp::Unknown(_) => "cmp?",
            };
            format!(
                "{} {} {}",
                format_inst(name, modifiers),
                format_dst(dst),
                format_srcs(&[src0.clone(), src1.clone()])
            )
        }
        IrOp::Select {
            dst,
            cond,
            src_ge,
            src_lt,
            modifiers,
        } => format!(
            "{} {} {}",
            format_inst("cmp", modifiers),
            format_dst(dst),
            format_srcs(&[cond.clone(), src_ge.clone(), src_lt.clone()])
        ),
        IrOp::Pow {
            dst,
            src0,
            src1,
            modifiers,
        } => format!(
            "{} {}",
            format_inst("pow", modifiers),
            format_dst_src(dst, &[src0.clone(), src1.clone()])
        ),
        IrOp::TexSample {
            kind,
            dst,
            coord,
            ddx,
            ddy,
            sampler,
            modifiers,
        } => {
            let opname = match kind {
                TexSampleKind::ImplicitLod { project: false } => "texld",
                TexSampleKind::ImplicitLod { project: true } => "texldp",
                TexSampleKind::Bias => "texldb",
                TexSampleKind::ExplicitLod => "texldl",
                TexSampleKind::Grad => "texldd",
            };
            let mut parts = vec![format_dst(dst), format_src(coord)];
            if let Some(ddx) = ddx {
                parts.push(format_src(ddx));
            }
            if let Some(ddy) = ddy {
                parts.push(format_src(ddy));
            }
            parts.push(format!("s{}", sampler));
            format!("{} {}", format_inst(opname, modifiers), parts.join(", "))
        }
    }
}

fn format_inst(name: &str, modifiers: &InstModifiers) -> String {
    let mut out = name.to_owned();
    if modifiers.saturate {
        out.push_str("_sat");
    }
    match modifiers.shift {
        ResultShift::None => {}
        ResultShift::Mul2 => out.push_str("_x2"),
        ResultShift::Mul4 => out.push_str("_x4"),
        ResultShift::Mul8 => out.push_str("_x8"),
        ResultShift::Div2 => out.push_str("_d2"),
        ResultShift::Div4 => out.push_str("_d4"),
        ResultShift::Div8 => out.push_str("_d8"),
        ResultShift::Unknown(v) => out.push_str(&format!("_shift{}", v)),
    }
    if let Some(pred) = &modifiers.predicate {
        out.push_str(&format!(
            " ({}{})",
            if pred.negate { "!" } else { "" },
            format_reg(&pred.reg)
        ));
    }
    if modifiers.coissue {
        out.push_str(" [coissue]");
    }
    out
}

fn format_dst_src(dst: &Dst, srcs: &[Src]) -> String {
    format!("{}, {}", format_dst(dst), format_srcs(srcs))
}

fn format_dst(dst: &Dst) -> String {
    let mut out = format_reg(&dst.reg);
    if dst.mask.0 != 0xF {
        out.push('.');
        out.push_str(&mask_to_string(dst.mask));
    }
    out
}

fn mask_to_string(mask: WriteMask) -> String {
    let mut s = String::new();
    for (c, ch) in [
        (SwizzleComponent::X, 'x'),
        (SwizzleComponent::Y, 'y'),
        (SwizzleComponent::Z, 'z'),
        (SwizzleComponent::W, 'w'),
    ] {
        if mask.contains(c) {
            s.push(ch);
        }
    }
    if s.is_empty() {
        "0".to_owned()
    } else {
        s
    }
}

fn format_srcs(srcs: &[Src]) -> String {
    srcs.iter().map(format_src).collect::<Vec<_>>().join(", ")
}

fn format_src(src: &Src) -> String {
    let mut out = String::new();
    match src.modifier {
        SrcModifier::None => {}
        SrcModifier::Negate => out.push('-'),
        SrcModifier::Bias => out.push_str("bias("),
        SrcModifier::BiasNegate => out.push_str("-bias("),
        SrcModifier::Sign => out.push_str("sign("),
        SrcModifier::SignNegate => out.push_str("-sign("),
        SrcModifier::Comp => out.push_str("comp("),
        SrcModifier::X2 => out.push_str("x2("),
        SrcModifier::X2Negate => out.push_str("-x2("),
        SrcModifier::Dz => out.push_str("dz("),
        SrcModifier::Dw => out.push_str("dw("),
        SrcModifier::Abs => out.push_str("abs("),
        SrcModifier::AbsNegate => out.push_str("-abs("),
        SrcModifier::Not => out.push_str("not("),
        SrcModifier::Unknown(m) => out.push_str(&format!("mod{m}(")),
    }

    out.push_str(&format_reg(&src.reg));

    if src.swizzle != Swizzle::identity() {
        out.push('.');
        out.push_str(&swizzle_to_string(src.swizzle));
    }

    match src.modifier {
        SrcModifier::None | SrcModifier::Negate => {}
        SrcModifier::Bias
        | SrcModifier::BiasNegate
        | SrcModifier::Sign
        | SrcModifier::SignNegate
        | SrcModifier::Comp
        | SrcModifier::X2
        | SrcModifier::X2Negate
        | SrcModifier::Dz
        | SrcModifier::Dw
        | SrcModifier::Abs
        | SrcModifier::AbsNegate
        | SrcModifier::Not
        | SrcModifier::Unknown(_) => out.push(')'),
    }
    out
}

fn swizzle_to_string(swizzle: Swizzle) -> String {
    swizzle
        .0
        .iter()
        .map(|c| match c {
            SwizzleComponent::X => 'x',
            SwizzleComponent::Y => 'y',
            SwizzleComponent::Z => 'z',
            SwizzleComponent::W => 'w',
        })
        .collect()
}

fn format_cond(cond: &Cond) -> String {
    match cond {
        Cond::NonZero { src } => format!("{} != 0", format_src(src)),
        Cond::Compare { op, src0, src1 } => format!(
            "{} {} {}",
            format_src(src0),
            op_to_str(*op),
            format_src(src1)
        ),
        Cond::Predicate { pred } => format!(
            "{}{}.{:?}",
            if pred.negate { "!" } else { "" },
            format_reg(&pred.reg),
            pred.component
        ),
    }
}

fn op_to_str(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Gt => ">",
        CompareOp::Ge => ">=",
        CompareOp::Eq => "==",
        CompareOp::Ne => "!=",
        CompareOp::Lt => "<",
        CompareOp::Le => "<=",
        CompareOp::Unknown(_) => "??",
    }
}
