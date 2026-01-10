use std::fmt;

use crate::sm3::decode::{ResultShift, SrcModifier, Swizzle, SwizzleComponent, TextureType, WriteMask};
use crate::sm3::types::ShaderVersion;

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderIr {
    pub version: ShaderVersion,
    pub inputs: Vec<IoDecl>,
    pub outputs: Vec<IoDecl>,
    pub samplers: Vec<SamplerDecl>,
    pub const_defs_f32: Vec<ConstDefF32>,
    pub body: Block,
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

#[derive(Debug, Clone, PartialEq)]
pub enum IrOp {
    Mov {
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
    Cmp {
        op: CompareOp,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    Color(u8),
    TexCoord(u8),
    Normal(u8),
    Fog(u8),
    PointSize(u8),
    Depth(u8),
    Other { usage: u8, index: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    /// `src.x != 0.0`
    NonZero { src: Src },
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
        writeln!(f, "{}_{}_{}", stage_prefix(self.version.stage), self.version.major, self.version.minor)?;
        if !self.inputs.is_empty() {
            writeln!(f, "inputs:")?;
            for input in &self.inputs {
                writeln!(f, "  {} = {}", format_reg(&input.reg), format_io_decl(input))?;
            }
        }
        if !self.outputs.is_empty() {
            writeln!(f, "outputs:")?;
            for output in &self.outputs {
                writeln!(f, "  {} = {}", format_reg(&output.reg), format_io_decl(output))?;
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
        Stmt::Loop { body } => {
            writeln!(f, "{}loop {{", pad)?;
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
        IrOp::Mov { dst, src, modifiers } => format!("{} {}", format_inst("mov", modifiers), format_dst_src(dst, &[src.clone()])),
        IrOp::Add { dst, src0, src1, modifiers } => format!("{} {}", format_inst("add", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone()])),
        IrOp::Sub { dst, src0, src1, modifiers } => format!("{} {}", format_inst("sub", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone()])),
        IrOp::Mul { dst, src0, src1, modifiers } => format!("{} {}", format_inst("mul", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone()])),
        IrOp::Mad { dst, src0, src1, src2, modifiers } => format!("{} {}", format_inst("mad", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone(), src2.clone()])),
        IrOp::Dp3 { dst, src0, src1, modifiers } => format!("{} {}", format_inst("dp3", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone()])),
        IrOp::Dp4 { dst, src0, src1, modifiers } => format!("{} {}", format_inst("dp4", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone()])),
        IrOp::Rcp { dst, src, modifiers } => format!("{} {}", format_inst("rcp", modifiers), format_dst_src(dst, &[src.clone()])),
        IrOp::Rsq { dst, src, modifiers } => format!("{} {}", format_inst("rsq", modifiers), format_dst_src(dst, &[src.clone()])),
        IrOp::Min { dst, src0, src1, modifiers } => format!("{} {}", format_inst("min", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone()])),
        IrOp::Max { dst, src0, src1, modifiers } => format!("{} {}", format_inst("max", modifiers), format_dst_src(dst, &[src0.clone(), src1.clone()])),
        IrOp::Cmp { op, dst, src0, src1, modifiers } => {
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
        IrOp::TexSample { kind, dst, coord, ddx, ddy, sampler, modifiers } => {
            let opname = match kind {
                TexSampleKind::ImplicitLod { project: false } => "texld",
                TexSampleKind::ImplicitLod { project: true } => "texldp",
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
        out.push_str(&format!(" ({}{})", if pred.negate { "!" } else { "" }, format_reg(&pred.reg)));
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
        SrcModifier::Abs => out.push_str("abs("),
        SrcModifier::AbsNegate => out.push_str("-abs("),
        SrcModifier::Unknown(m) => out.push_str(&format!("mod{m}(")),
    }

    out.push_str(&format_reg(&src.reg));

    if src.swizzle != Swizzle::identity() {
        out.push('.');
        out.push_str(&swizzle_to_string(src.swizzle));
    }

    match src.modifier {
        SrcModifier::Abs | SrcModifier::AbsNegate | SrcModifier::Unknown(_) => out.push(')'),
        _ => {}
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
        Cond::Compare { op, src0, src1 } => format!("{} {} {}", format_src(src0), op_to_str(*op), format_src(src1)),
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
