use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::sm3::decode::{ResultShift, SrcModifier, Swizzle, SwizzleComponent, WriteMask};
use crate::sm3::ir::{Block, CompareOp, Cond, Dst, InstModifiers, IrOp, RegFile, RegRef, Src, Stmt};
use crate::sm3::types::ShaderStage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgslError {
    pub message: String,
}

impl std::fmt::Display for WgslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WGSL generation error: {}", self.message)
    }
}

impl std::error::Error for WgslError {}

fn err(message: impl Into<String>) -> WgslError {
    WgslError {
        message: message.into(),
    }
}

#[derive(Debug, Clone)]
pub struct WgslOutput {
    pub wgsl: String,
    pub entry_point: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarTy {
    F32,
    I32,
    Bool,
}

impl ScalarTy {
    fn wgsl_scalar(self) -> &'static str {
        match self {
            ScalarTy::F32 => "f32",
            ScalarTy::I32 => "i32",
            ScalarTy::Bool => "bool",
        }
    }

    fn wgsl_vec4(self) -> String {
        format!("vec4<{}>", self.wgsl_scalar())
    }
}

fn reg_scalar_ty(file: RegFile) -> Option<ScalarTy> {
    match file {
        // Float register files.
        RegFile::Temp
        | RegFile::Input
        | RegFile::Const
        | RegFile::Texture
        | RegFile::RastOut
        | RegFile::AttrOut
        | RegFile::TexCoordOut
        | RegFile::Output
        | RegFile::ColorOut
        | RegFile::DepthOut
        | RegFile::MiscType => Some(ScalarTy::F32),

        // Integer-ish register files.
        RegFile::Addr | RegFile::ConstInt | RegFile::Loop | RegFile::Label => Some(ScalarTy::I32),

        // Boolean register files.
        RegFile::Predicate | RegFile::ConstBool => Some(ScalarTy::Bool),

        // Samplers are handled via the `sampler` field in `TexSample` ops.
        RegFile::Sampler => None,
    }
}

fn reg_var_name(reg: &RegRef) -> Result<String, WgslError> {
    if reg.relative.is_some() {
        return Err(err("relative register addressing is not supported in WGSL lowering"));
    }
    Ok(match reg.file {
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
    })
}

fn swizzle_suffix(swz: Swizzle) -> Option<String> {
    let comp = |c: SwizzleComponent| match c {
        SwizzleComponent::X => 'x',
        SwizzleComponent::Y => 'y',
        SwizzleComponent::Z => 'z',
        SwizzleComponent::W => 'w',
    };
    let s: String = swz.0.into_iter().map(comp).collect();
    if s == "xyzw" {
        None
    } else {
        Some(format!(".{}", s))
    }
}

fn mask_suffix(mask: WriteMask) -> Option<String> {
    if mask.0 == 0xF {
        return None;
    }
    let mut s = String::new();
    if mask.contains(SwizzleComponent::X) {
        s.push('x');
    }
    if mask.contains(SwizzleComponent::Y) {
        s.push('y');
    }
    if mask.contains(SwizzleComponent::Z) {
        s.push('z');
    }
    if mask.contains(SwizzleComponent::W) {
        s.push('w');
    }
    if s.is_empty() {
        None
    } else {
        Some(format!(".{}", s))
    }
}

fn default_vec4(ty: ScalarTy) -> &'static str {
    match ty {
        ScalarTy::F32 => "vec4<f32>(0.0)",
        ScalarTy::I32 => "vec4<i32>(0)",
        ScalarTy::Bool => "vec4<bool>(false)",
    }
}

struct RegUsage {
    temps: BTreeSet<u32>,
    outputs: BTreeSet<(RegFile, u32)>,
    predicates: BTreeSet<u32>,
    float_consts: BTreeSet<u32>,
    int_consts: BTreeSet<u32>,
    bool_consts: BTreeSet<u32>,
}

impl RegUsage {
    fn new() -> Self {
        Self {
            temps: BTreeSet::new(),
            outputs: BTreeSet::new(),
            predicates: BTreeSet::new(),
            float_consts: BTreeSet::new(),
            int_consts: BTreeSet::new(),
            bool_consts: BTreeSet::new(),
        }
    }
}

fn collect_reg_usage(block: &Block, usage: &mut RegUsage) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Op(op) => collect_op_usage(op, usage),
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                collect_cond_usage(cond, usage);
                collect_reg_usage(then_block, usage);
                if let Some(else_block) = else_block {
                    collect_reg_usage(else_block, usage);
                }
            }
            Stmt::Loop { body } => collect_reg_usage(body, usage),
            Stmt::Break => {}
            Stmt::BreakIf { cond } => collect_cond_usage(cond, usage),
            Stmt::Discard { src } => collect_src_usage(src, usage),
        }
    }
}

fn collect_op_usage(op: &IrOp, usage: &mut RegUsage) {
    // Predicate modifier usage.
    if let Some(pred) = &op_modifiers(op).predicate {
        collect_reg_ref_usage(&pred.reg, usage);
    }

    match op {
        IrOp::Mov { dst, src, .. }
        | IrOp::Rcp { dst, src, .. }
        | IrOp::Rsq { dst, src, .. }
        | IrOp::Frc { dst, src, .. } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src, usage);
        }
        IrOp::Add { dst, src0, src1, .. }
        | IrOp::Sub { dst, src0, src1, .. }
        | IrOp::Mul { dst, src0, src1, .. }
        | IrOp::Min { dst, src0, src1, .. }
        | IrOp::Max { dst, src0, src1, .. }
        | IrOp::Dp3 { dst, src0, src1, .. }
        | IrOp::Dp4 { dst, src0, src1, .. }
        | IrOp::SetCmp { dst, src0, src1, .. } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
        }
        IrOp::Select {
            dst,
            cond,
            src_ge,
            src_lt,
            ..
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(cond, usage);
            collect_src_usage(src_ge, usage);
            collect_src_usage(src_lt, usage);
        }
        IrOp::Mad {
            dst,
            src0,
            src1,
            src2,
            ..
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
            collect_src_usage(src2, usage);
        }
        IrOp::TexSample {
            dst,
            coord,
            ddx,
            ddy,
            ..
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(coord, usage);
            if let Some(ddx) = ddx {
                collect_src_usage(ddx, usage);
            }
            if let Some(ddy) = ddy {
                collect_src_usage(ddy, usage);
            }
        }
    }
}

fn collect_cond_usage(cond: &Cond, usage: &mut RegUsage) {
    match cond {
        Cond::NonZero { src } => collect_src_usage(src, usage),
        Cond::Compare { src0, src1, .. } => {
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
        }
        Cond::Predicate { pred } => collect_reg_ref_usage(&pred.reg, usage),
    }
}

fn collect_dst_usage(dst: &Dst, usage: &mut RegUsage) {
    collect_reg_ref_usage(&dst.reg, usage);
}

fn collect_src_usage(src: &Src, usage: &mut RegUsage) {
    collect_reg_ref_usage(&src.reg, usage);
}

fn collect_reg_ref_usage(reg: &RegRef, usage: &mut RegUsage) {
    match reg.file {
        RegFile::Temp => {
            usage.temps.insert(reg.index);
        }
        RegFile::Predicate => {
            usage.predicates.insert(reg.index);
        }
        RegFile::ColorOut | RegFile::DepthOut | RegFile::RastOut | RegFile::AttrOut
        | RegFile::TexCoordOut | RegFile::Output => {
            usage.outputs.insert((reg.file, reg.index));
        }
        RegFile::Const => {
            usage.float_consts.insert(reg.index);
        }
        RegFile::ConstInt => {
            usage.int_consts.insert(reg.index);
        }
        RegFile::ConstBool => {
            usage.bool_consts.insert(reg.index);
        }
        _ => {
            // Other register files are either not represented in WGSL lowering yet
            // or are declared opportunistically when needed (e.g. inputs).
        }
    }
    if let Some(rel) = &reg.relative {
        collect_reg_ref_usage(&rel.reg, usage);
    }
}

fn op_modifiers(op: &IrOp) -> &InstModifiers {
    match op {
        IrOp::Mov { modifiers, .. }
        | IrOp::Add { modifiers, .. }
        | IrOp::Sub { modifiers, .. }
        | IrOp::Mul { modifiers, .. }
        | IrOp::Mad { modifiers, .. }
        | IrOp::Dp3 { modifiers, .. }
        | IrOp::Dp4 { modifiers, .. }
        | IrOp::Rcp { modifiers, .. }
        | IrOp::Rsq { modifiers, .. }
        | IrOp::Frc { modifiers, .. }
        | IrOp::Min { modifiers, .. }
        | IrOp::Max { modifiers, .. }
        | IrOp::SetCmp { modifiers, .. }
        | IrOp::Select { modifiers, .. }
        | IrOp::TexSample { modifiers, .. } => modifiers,
    }
}

fn format_f32(v: f32) -> String {
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

fn src_expr(src: &Src) -> Result<(String, ScalarTy), WgslError> {
    let ty = reg_scalar_ty(src.reg.file).ok_or_else(|| err("unsupported source register file"))?;
    let mut expr = reg_var_name(&src.reg)?;
    if let Some(swz) = swizzle_suffix(src.swizzle) {
        expr.push_str(&swz);
    }

    expr = match (ty, src.modifier) {
        (_, SrcModifier::None) => expr,
        (ScalarTy::F32, SrcModifier::Negate) | (ScalarTy::I32, SrcModifier::Negate) => {
            format!("-({expr})")
        }
        (ScalarTy::Bool, SrcModifier::Negate) => format!("!({expr})"),
        (ScalarTy::F32, SrcModifier::Bias) => format!("(({expr}) - vec4<f32>(0.5))"),
        (ScalarTy::F32, SrcModifier::BiasNegate) => format!("-(({expr}) - vec4<f32>(0.5))"),
        (ScalarTy::F32, SrcModifier::Sign) => format!("(({expr}) * 2.0 - vec4<f32>(1.0))"),
        (ScalarTy::F32, SrcModifier::SignNegate) => {
            format!("-(({expr}) * 2.0 - vec4<f32>(1.0))")
        }
        (ScalarTy::F32, SrcModifier::Comp) | (ScalarTy::F32, SrcModifier::Not) => {
            format!("(vec4<f32>(1.0) - ({expr}))")
        }
        (ScalarTy::F32, SrcModifier::X2) => format!("(({expr}) * 2.0)"),
        (ScalarTy::F32, SrcModifier::X2Negate) => format!("-(({expr}) * 2.0)"),
        (ScalarTy::F32, SrcModifier::Dz) => format!("(({expr}) / ({expr}).z)"),
        (ScalarTy::F32, SrcModifier::Dw) => format!("(({expr}) / ({expr}).w)"),
        (ScalarTy::F32, SrcModifier::Abs) | (ScalarTy::I32, SrcModifier::Abs) => {
            format!("abs({expr})")
        }
        (ScalarTy::Bool, SrcModifier::Abs) => return Err(err("abs on bool source")),
        (ScalarTy::F32, SrcModifier::AbsNegate) | (ScalarTy::I32, SrcModifier::AbsNegate) => {
            format!("-abs({expr})")
        }
        (ScalarTy::Bool, SrcModifier::AbsNegate) => return Err(err("absnegate on bool source")),
        (ScalarTy::I32, SrcModifier::Bias)
        | (ScalarTy::I32, SrcModifier::BiasNegate)
        | (ScalarTy::I32, SrcModifier::Sign)
        | (ScalarTy::I32, SrcModifier::SignNegate)
        | (ScalarTy::I32, SrcModifier::Comp)
        | (ScalarTy::I32, SrcModifier::X2)
        | (ScalarTy::I32, SrcModifier::X2Negate)
        | (ScalarTy::I32, SrcModifier::Dz)
        | (ScalarTy::I32, SrcModifier::Dw)
        | (ScalarTy::I32, SrcModifier::Not) => {
            return Err(err("float-only source modifier used on integer source"))
        }
        (ScalarTy::Bool, SrcModifier::Bias)
        | (ScalarTy::Bool, SrcModifier::BiasNegate)
        | (ScalarTy::Bool, SrcModifier::Sign)
        | (ScalarTy::Bool, SrcModifier::SignNegate)
        | (ScalarTy::Bool, SrcModifier::Comp)
        | (ScalarTy::Bool, SrcModifier::X2)
        | (ScalarTy::Bool, SrcModifier::X2Negate)
        | (ScalarTy::Bool, SrcModifier::Dz)
        | (ScalarTy::Bool, SrcModifier::Dw)
        | (ScalarTy::Bool, SrcModifier::Not) => {
            return Err(err("float-only source modifier used on boolean source"))
        }
        (_, SrcModifier::Unknown(_)) => return Err(err("unknown source modifier")),
    };

    Ok((expr, ty))
}

fn cond_expr(cond: &Cond) -> Result<String, WgslError> {
    match cond {
        Cond::NonZero { src } => {
            let (expr, ty) = src_expr(src)?;
            Ok(match ty {
                ScalarTy::F32 => format!("({expr}.x != 0.0)"),
                ScalarTy::I32 => format!("({expr}.x != 0)"),
                ScalarTy::Bool => format!("{expr}.x"),
            })
        }
        Cond::Compare { op, src0, src1 } => {
            let (a, aty) = src_expr(src0)?;
            let (b, bty) = src_expr(src1)?;
            if aty != bty {
                return Err(err("comparison between mismatched types"));
            }

            let op_str = match op {
                CompareOp::Gt => ">",
                CompareOp::Ge => ">=",
                CompareOp::Eq => "==",
                CompareOp::Ne => "!=",
                CompareOp::Lt => "<",
                CompareOp::Le => "<=",
                CompareOp::Unknown(_) => return Err(err("unknown comparison op")),
            };
            Ok(format!("({a}.x {op_str} {b}.x)"))
        }
        Cond::Predicate { pred } => {
            let mut e = reg_var_name(&pred.reg)?;
            e.push('.');
            e.push(match pred.component {
                SwizzleComponent::X => 'x',
                SwizzleComponent::Y => 'y',
                SwizzleComponent::Z => 'z',
                SwizzleComponent::W => 'w',
            });
            if pred.negate {
                Ok(format!("!({e})"))
            } else {
                Ok(e)
            }
        }
    }
}

fn apply_float_result_modifiers(expr: String, mods: &InstModifiers) -> Result<String, WgslError> {
    let mut out = expr;
    out = match mods.shift {
        ResultShift::None => out,
        ResultShift::Mul2 => format!("({out}) * 2.0"),
        ResultShift::Mul4 => format!("({out}) * 4.0"),
        ResultShift::Mul8 => format!("({out}) * 8.0"),
        ResultShift::Div2 => format!("({out}) / 2.0"),
        ResultShift::Div4 => format!("({out}) / 4.0"),
        ResultShift::Div8 => format!("({out}) / 8.0"),
        ResultShift::Unknown(v) => return Err(err(format!("unknown result shift modifier {v}"))),
    };
    if mods.saturate {
        out = format!("clamp({out}, vec4<f32>(0.0), vec4<f32>(1.0))");
    }
    Ok(out)
}

fn predicate_ref_expr(pred: &crate::sm3::ir::PredicateRef) -> Result<String, WgslError> {
    let mut e = reg_var_name(&pred.reg)?;
    e.push('.');
    e.push(match pred.component {
        SwizzleComponent::X => 'x',
        SwizzleComponent::Y => 'y',
        SwizzleComponent::Z => 'z',
        SwizzleComponent::W => 'w',
    });
    if pred.negate {
        Ok(format!("!({e})"))
    } else {
        Ok(e)
    }
}

fn emit_op_line(op: &IrOp) -> Result<String, WgslError> {
    match op {
        IrOp::Mov { dst, src, modifiers } => {
            let (src_e, src_ty) = src_expr(src)?;
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if src_ty != dst_ty {
                return Err(err("mov between mismatched types"));
            }
            let src_e = match dst_ty {
                ScalarTy::F32 => apply_float_result_modifiers(src_e, modifiers)?,
                _ => src_e,
            };
            emit_assign(dst, src_e)
        }
        IrOp::Add { dst, src0, src1, modifiers } => {
            emit_float_binop(dst, src0, src1, modifiers, "+")
        }
        IrOp::Sub { dst, src0, src1, modifiers } => {
            emit_float_binop(dst, src0, src1, modifiers, "-")
        }
        IrOp::Mul { dst, src0, src1, modifiers } => {
            emit_float_binop(dst, src0, src1, modifiers, "*")
        }
        IrOp::Mad {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0)?;
            let (b, bty) = src_expr(src1)?;
            let (c, cty) = src_expr(src2)?;
            if aty != ScalarTy::F32 || bty != ScalarTy::F32 || cty != ScalarTy::F32 {
                return Err(err("mad only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("mad destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("(({a}) * ({b})) + ({c})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Min { dst, src0, src1, modifiers } => {
            emit_float_func2(dst, src0, src1, modifiers, "min")
        }
        IrOp::Max { dst, src0, src1, modifiers } => {
            emit_float_func2(dst, src0, src1, modifiers, "max")
        }
        IrOp::Rcp { dst, src, modifiers } => {
            let (s, ty) = src_expr(src)?;
            if ty != ScalarTy::F32 {
                return Err(err("rcp only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("rcp destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("(vec4<f32>(1.0) / ({s}))"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Rsq { dst, src, modifiers } => {
            let (s, ty) = src_expr(src)?;
            if ty != ScalarTy::F32 {
                return Err(err("rsq only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("rsq destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("inverseSqrt({s})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Frc { dst, src, modifiers } => {
            let (s, ty) = src_expr(src)?;
            if ty != ScalarTy::F32 {
                return Err(err("frc only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("frc destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("fract({s})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Dp3 { dst, src0, src1, modifiers } => {
            let (a, aty) = src_expr(src0)?;
            let (b, bty) = src_expr(src1)?;
            if aty != ScalarTy::F32 || bty != ScalarTy::F32 {
                return Err(err("dp3 only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dp3 destination must be float"));
            }
            let dot = format!("dot(({a}).xyz, ({b}).xyz)");
            let e = apply_float_result_modifiers(format!("vec4<f32>({dot})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Dp4 { dst, src0, src1, modifiers } => {
            let (a, aty) = src_expr(src0)?;
            let (b, bty) = src_expr(src1)?;
            if aty != ScalarTy::F32 || bty != ScalarTy::F32 {
                return Err(err("dp4 only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dp4 destination must be float"));
            }
            let dot = format!("dot({a}, {b})");
            let e = apply_float_result_modifiers(format!("vec4<f32>({dot})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::SetCmp {
            op,
            dst,
            src0,
            src1,
            modifiers,
        } => {
            // `sge/slt/seq/sne`-style compare producing float mask: 1.0/0.0.
            let (a, aty) = src_expr(src0)?;
            let (b, bty) = src_expr(src1)?;
            if aty != bty {
                return Err(err("comparison between mismatched types"));
            }
            let op_str = match op {
                CompareOp::Gt => ">",
                CompareOp::Ge => ">=",
                CompareOp::Eq => "==",
                CompareOp::Ne => "!=",
                CompareOp::Lt => "<",
                CompareOp::Le => "<=",
                CompareOp::Unknown(_) => return Err(err("unknown comparison op")),
            };
            let cmp_expr = format!("({a} {op_str} {b})");

            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            match dst_ty {
                ScalarTy::F32 => {
                    if aty != ScalarTy::F32 {
                        return Err(err("setcmp only supports float sources in WGSL lowering"));
                    }
                    let e = apply_float_result_modifiers(
                        format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {cmp_expr})"),
                        modifiers,
                    )?;
                    emit_assign(dst, e)
                }
                ScalarTy::Bool => {
                    // `setp` writes predicate registers; result modifiers are meaningless.
                    if modifiers.saturate || modifiers.shift != ResultShift::None {
                        return Err(err("result modifiers not supported for predicate writes"));
                    }
                    if aty == ScalarTy::Bool
                        && !matches!(op, CompareOp::Eq | CompareOp::Ne)
                    {
                        return Err(err("ordered comparison on bool source"));
                    }
                    emit_assign(dst, cmp_expr)
                }
                ScalarTy::I32 => Err(err("setcmp destination cannot be integer")),
            }
        }
        IrOp::Select {
            dst,
            cond,
            src_ge,
            src_lt,
            modifiers,
        } => {
            let (cond_e, cond_ty) = src_expr(cond)?;
            let (a, aty) = src_expr(src_ge)?;
            let (b, bty) = src_expr(src_lt)?;
            if cond_ty != ScalarTy::F32 || aty != ScalarTy::F32 || bty != ScalarTy::F32 {
                return Err(err("select only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("select destination must be float"));
            }
            // D3D9 `cmp`: per-component select `cond >= 0 ? src_ge : src_lt`.
            let e = apply_float_result_modifiers(
                format!("select({b}, {a}, ({cond_e} >= vec4<f32>(0.0)))"),
                modifiers,
            )?;
            emit_assign(dst, e)
        }
        IrOp::TexSample { .. } => Err(err("texture sampling not supported in WGSL lowering")),
    }
}

fn emit_float_binop(
    dst: &Dst,
    src0: &Src,
    src1: &Src,
    modifiers: &InstModifiers,
    op: &str,
) -> Result<String, WgslError> {
    let (a, aty) = src_expr(src0)?;
    let (b, bty) = src_expr(src1)?;
    if aty != ScalarTy::F32 || bty != ScalarTy::F32 {
        return Err(err("float arithmetic uses non-float source"));
    }
    let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
    if dst_ty != ScalarTy::F32 {
        return Err(err("float arithmetic destination must be float"));
    }
    let e = apply_float_result_modifiers(format!("({a}) {op} ({b})"), modifiers)?;
    emit_assign(dst, e)
}

fn emit_float_func2(
    dst: &Dst,
    src0: &Src,
    src1: &Src,
    modifiers: &InstModifiers,
    func: &str,
) -> Result<String, WgslError> {
    let (a, aty) = src_expr(src0)?;
    let (b, bty) = src_expr(src1)?;
    if aty != ScalarTy::F32 || bty != ScalarTy::F32 {
        return Err(err("float arithmetic uses non-float source"));
    }
    let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
    if dst_ty != ScalarTy::F32 {
        return Err(err("float arithmetic destination must be float"));
    }
    let e = apply_float_result_modifiers(format!("{func}(({a}), ({b}))"), modifiers)?;
    emit_assign(dst, e)
}

fn emit_assign(dst: &Dst, value: String) -> Result<String, WgslError> {
    let dst_name = reg_var_name(&dst.reg)?;
    if let Some(mask) = mask_suffix(dst.mask) {
        Ok(format!(
            "{dst_name}{mask} = ({value}){mask};",
            dst_name = dst_name,
            mask = mask,
            value = value
        ))
    } else {
        Ok(format!("{dst_name} = {value};"))
    }
}

fn emit_block(wgsl: &mut String, block: &Block, indent: usize) -> Result<(), WgslError> {
    for stmt in &block.stmts {
        emit_stmt(wgsl, stmt, indent)?;
    }
    Ok(())
}

fn emit_stmt(wgsl: &mut String, stmt: &Stmt, indent: usize) -> Result<(), WgslError> {
    let pad = "  ".repeat(indent);
    match stmt {
        Stmt::Op(op) => {
            if let Some(pred) = &op_modifiers(op).predicate {
                let pred_cond = predicate_ref_expr(pred)?;
                let _ = writeln!(wgsl, "{pad}if ({pred_cond}) {{");
                let line = emit_op_line(op)?;
                let inner_pad = "  ".repeat(indent + 1);
                let _ = writeln!(wgsl, "{inner_pad}{line}");
                let _ = writeln!(wgsl, "{pad}}}");
            } else {
                let line = emit_op_line(op)?;
                let _ = writeln!(wgsl, "{pad}{line}");
            }
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
        } => {
            let cond = cond_expr(cond)?;
            let _ = writeln!(wgsl, "{pad}if ({cond}) {{");
            emit_block(wgsl, then_block, indent + 1)?;
            if let Some(else_block) = else_block {
                let _ = writeln!(wgsl, "{pad}}} else {{");
                emit_block(wgsl, else_block, indent + 1)?;
            }
            let _ = writeln!(wgsl, "{pad}}}");
        }
        Stmt::Loop { body } => {
            let _ = writeln!(wgsl, "{pad}loop {{");
            emit_block(wgsl, body, indent + 1)?;
            let _ = writeln!(wgsl, "{pad}}}");
        }
        Stmt::Break => {
            let _ = writeln!(wgsl, "{pad}break;");
        }
        Stmt::BreakIf { cond } => {
            let cond = cond_expr(cond)?;
            let _ = writeln!(wgsl, "{pad}if ({cond}) {{ break; }}");
        }
        Stmt::Discard { .. } => {
            // texkill semantics are more complex than a straight unconditional discard.
            let _ = writeln!(wgsl, "{pad}discard;");
        }
    }
    Ok(())
}

pub fn generate_wgsl(ir: &crate::sm3::ir::ShaderIr) -> Result<WgslOutput, WgslError> {
    // Collect usage so we can declare required locals and constant defs.
    let mut usage = RegUsage::new();
    collect_reg_usage(&ir.body, &mut usage);

    let mut f32_defs: BTreeMap<u32, [f32; 4]> = BTreeMap::new();
    for def in &ir.const_defs_f32 {
        f32_defs.insert(def.index, def.value);
    }

    let mut i32_defs: BTreeMap<u32, [i32; 4]> = BTreeMap::new();
    for def in &ir.const_defs_i32 {
        i32_defs.insert(def.index, def.value);
    }

    let mut bool_defs: BTreeMap<u32, bool> = BTreeMap::new();
    for def in &ir.const_defs_bool {
        bool_defs.insert(def.index, def.value);
    }

    let mut wgsl = String::new();

    let entry_point = match ir.version.stage {
        ShaderStage::Vertex => "vs_main",
        ShaderStage::Pixel => "fs_main",
    };

    // Fragment output struct (even if only one output) keeps codegen simple.
    if ir.version.stage == ShaderStage::Pixel {
        wgsl.push_str("struct FsOut {\n");
        if usage.outputs.iter().any(|(f, _)| *f == RegFile::ColorOut) {
            for (file, index) in &usage.outputs {
                if *file != RegFile::ColorOut {
                    continue;
                }
                let _ = writeln!(wgsl, "  @location({}) oC{}: vec4<f32>,", index, index);
            }
        } else {
            wgsl.push_str("  @location(0) oC0: vec4<f32>,\n");
        }
        wgsl.push_str("};\n\n");
    }

    match ir.version.stage {
        ShaderStage::Vertex => {
            wgsl.push_str("@vertex\nfn vs_main() -> @builtin(position) vec4<f32> {\n");
        }
        ShaderStage::Pixel => {
            wgsl.push_str("@fragment\nfn fs_main() -> FsOut {\n");
        }
    }

    // Local temp registers.
    for r in &usage.temps {
        let _ = writeln!(wgsl, "  var r{r}: vec4<f32> = vec4<f32>(0.0);");
    }

    // Predicate registers.
    for p in &usage.predicates {
        let _ = writeln!(wgsl, "  var p{p}: vec4<bool> = vec4<bool>(false);");
    }

    // Outputs used by the shader. These are mutable locals that get copied into the function
    // return value at the end.
    for (file, index) in &usage.outputs {
        let reg = RegRef {
            file: *file,
            index: *index,
            relative: None,
        };
        let ty = reg_scalar_ty(*file).unwrap_or(ScalarTy::F32);
        let name = reg_var_name(&reg)?;
        let _ = writeln!(wgsl, "  var {name}: {} = {};", ty.wgsl_vec4(), default_vec4(ty));
    }

    // Ensure at least oC0 exists for fragment shaders (common case).
    if ir.version.stage == ShaderStage::Pixel
        && !usage.outputs.iter().any(|(f, i)| *f == RegFile::ColorOut && *i == 0)
    {
        wgsl.push_str("  var oC0: vec4<f32> = vec4<f32>(0.0);\n");
    }

    // Embedded float constants (`def c#`).
    for idx in &usage.float_consts {
        let value = f32_defs.get(idx).copied().unwrap_or([0.0; 4]);
        let _ = writeln!(
            wgsl,
            "  let c{idx}: vec4<f32> = vec4<f32>({}, {}, {}, {});",
            format_f32(value[0]),
            format_f32(value[1]),
            format_f32(value[2]),
            format_f32(value[3])
        );
    }

    // Embedded integer constants (`defi i#`).
    for idx in &usage.int_consts {
        let value = i32_defs.get(idx).copied().unwrap_or([0; 4]);
        let _ = writeln!(
            wgsl,
            "  let i{idx}: vec4<i32> = vec4<i32>({}, {}, {}, {});",
            value[0], value[1], value[2], value[3]
        );
    }

    // Embedded boolean constants (`defb b#`). D3D bool regs are scalar; we splat across vec4 for
    // register-like access with swizzles.
    for idx in &usage.bool_consts {
        let v = bool_defs.get(idx).copied().unwrap_or(false);
        let _ = writeln!(
            wgsl,
            "  let b{idx}: vec4<bool> = vec4<bool>({v}, {v}, {v}, {v});"
        );
    }

    wgsl.push('\n');

    emit_block(&mut wgsl, &ir.body, 1)?;

    match ir.version.stage {
        ShaderStage::Vertex => {
            // Position output: prefer oPos, otherwise just emit a zero vector.
            if usage
                .outputs
                .iter()
                .any(|(f, _)| *f == RegFile::RastOut)
            {
                wgsl.push_str("  return oPos;\n");
            } else {
                wgsl.push_str("  return vec4<f32>(0.0);\n");
            }
            wgsl.push_str("}\n");
        }
        ShaderStage::Pixel => {
            wgsl.push_str("  var out: FsOut;\n");
            if usage.outputs.iter().any(|(f, _)| *f == RegFile::ColorOut) {
                for (file, index) in &usage.outputs {
                    if *file != RegFile::ColorOut {
                        continue;
                    }
                    let _ = writeln!(wgsl, "  out.oC{} = oC{};", index, index);
                }
            } else {
                wgsl.push_str("  out.oC0 = oC0;\n");
            }
            wgsl.push_str("  return out;\n}\n");
        }
    }

    Ok(WgslOutput { wgsl, entry_point })
}
