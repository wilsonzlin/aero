use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::sm3::decode::{ResultShift, SrcModifier, Swizzle, SwizzleComponent};
use crate::sm3::ir::{
    Block, CompareOp, Cond, Dst, InstModifiers, IrOp, PredicateRef, RegFile, RegRef, Semantic, Src,
    Stmt,
};
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
        return Err(err(
            "relative register addressing is not supported in WGSL lowering",
        ));
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

fn default_vec4(ty: ScalarTy) -> &'static str {
    match ty {
        ScalarTy::F32 => "vec4<f32>(0.0)",
        ScalarTy::I32 => "vec4<i32>(0)",
        ScalarTy::Bool => "vec4<bool>(false)",
    }
}

/// WebGPU guarantees support for at least 16 user-defined inter-stage locations (0..15).
///
/// We keep our D3D9 varying mapping within this bound so that shaders validate on all WebGPU
/// implementations.
const WEBGPU_MIN_INTER_STAGE_LOCATIONS: u32 = 16;

/// WebGPU guarantees support for at least 16 vertex input attributes (0..15).
const WEBGPU_MIN_VERTEX_ATTRIBUTES: u32 = 16;

/// Base location used for non-color / non-texcoord varyings when we can't derive a legacy mapping.
///
/// Locations are reserved as:
/// - 0..=3  : COLOR0..COLOR3
/// - 4..=11 : TEXCOORD0..TEXCOORD7
/// - 12..=15: other varyings (fallback, derived from register index)
const OTHER_VARYING_LOCATION_BASE: u32 = 12;

/// Determine the WGSL `@location(n)` to use for an inter-stage varying.
///
/// This is intentionally shared between the vertex and pixel shader stages so that separately
/// compiled shaders use matching locations.
fn varying_location(
    file: RegFile,
    index: u32,
    semantic: Option<&Semantic>,
) -> Result<u32, WgslError> {
    let loc = match file {
        // Legacy D3D9 VS outputs.
        RegFile::AttrOut => index,
        RegFile::TexCoordOut => 4 + index,

        // Legacy D3D9 PS inputs.
        RegFile::Texture => 4 + index,

        // SM3 generic VS outputs.
        RegFile::Output => match semantic {
            Some(Semantic::Color(i)) => u32::from(*i),
            Some(Semantic::TexCoord(i)) => 4 + u32::from(*i),
            _ => OTHER_VARYING_LOCATION_BASE + index,
        },

        // SM3 flexible PS inputs (a `v#` register can declare TEXCOORD semantics).
        RegFile::Input => match semantic {
            Some(Semantic::Color(i)) => u32::from(*i),
            Some(Semantic::TexCoord(i)) => 4 + u32::from(*i),
            _ => index,
        },

        _ => {
            return Err(err(format!(
                "register file {file:?} cannot be used as an inter-stage varying"
            )))
        }
    };

    if loc >= WEBGPU_MIN_INTER_STAGE_LOCATIONS {
        return Err(err(format!(
            "varying location {loc} exceeds the WebGPU minimum inter-stage location limit ({WEBGPU_MIN_INTER_STAGE_LOCATIONS})"
        )));
    }

    Ok(loc)
}

struct RegUsage {
    temps: BTreeSet<u32>,
    addrs: BTreeSet<u32>,
    inputs: BTreeSet<(RegFile, u32)>,
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
            addrs: BTreeSet::new(),
            inputs: BTreeSet::new(),
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
        IrOp::Mov { dst, src, modifiers }
        | IrOp::Mova { dst, src, modifiers }
        | IrOp::Rcp { dst, src, modifiers }
        | IrOp::Rsq { dst, src, modifiers }
        | IrOp::Frc { dst, src, modifiers }
        | IrOp::Exp { dst, src, modifiers }
        | IrOp::Log { dst, src, modifiers }
        | IrOp::Ddx { dst, src, modifiers }
        | IrOp::Ddy { dst, src, modifiers } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src, usage);
            collect_mods_usage(modifiers, usage);
        }
        IrOp::Add { dst, src0, src1, modifiers }
        | IrOp::Sub { dst, src0, src1, modifiers }
        | IrOp::Mul { dst, src0, src1, modifiers }
        | IrOp::Min { dst, src0, src1, modifiers }
        | IrOp::Max { dst, src0, src1, modifiers }
        | IrOp::Dp2 { dst, src0, src1, modifiers }
        | IrOp::Dp3 { dst, src0, src1, modifiers }
        | IrOp::Dp4 { dst, src0, src1, modifiers }
        | IrOp::SetCmp { dst, src0, src1, modifiers, .. }
        | IrOp::Pow { dst, src0, src1, modifiers } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
            collect_mods_usage(modifiers, usage);
        }
        IrOp::Select {
            dst,
            cond,
            src_ge,
            src_lt,
            modifiers,
            ..
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(cond, usage);
            collect_src_usage(src_ge, usage);
            collect_src_usage(src_lt, usage);
            collect_mods_usage(modifiers, usage);
        }
        IrOp::Mad {
            dst,
            src0,
            src1,
            src2,
            modifiers,
            ..
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
            collect_src_usage(src2, usage);
            collect_mods_usage(modifiers, usage);
        }
        IrOp::Lrp {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
            collect_src_usage(src2, usage);
            collect_mods_usage(modifiers, usage);
        }
        IrOp::TexSample {
            dst,
            coord,
            ddx,
            ddy,
            modifiers,
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
            collect_mods_usage(modifiers, usage);
        }
    }
}

fn collect_mods_usage(mods: &InstModifiers, usage: &mut RegUsage) {
    if let Some(pred) = &mods.predicate {
        collect_reg_ref_usage(&pred.reg, usage);
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
        RegFile::Addr => {
            usage.addrs.insert(reg.index);
        }
        RegFile::Input | RegFile::Texture => {
            usage.inputs.insert((reg.file, reg.index));
        }
        RegFile::Predicate => {
            usage.predicates.insert(reg.index);
        }
        RegFile::ColorOut
        | RegFile::DepthOut
        | RegFile::RastOut
        | RegFile::AttrOut
        | RegFile::TexCoordOut
        | RegFile::Output => {
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

fn src_expr(
    src: &Src,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
) -> Result<(String, ScalarTy), WgslError> {
    let ty = reg_scalar_ty(src.reg.file).ok_or_else(|| err("unsupported source register file"))?;
    let mut expr = if src.reg.file == RegFile::Const {
        // Relative constant addressing (`cN[a0.x]`) is represented via `RegRef.relative`.
        if let Some(rel) = &src.reg.relative {
            let rel_reg = reg_var_name(&rel.reg)?;
            if rel.reg.file != RegFile::Addr {
                return Err(err("relative constant addressing requires an address register"));
            }
            let comp = match rel.component {
                SwizzleComponent::X => 'x',
                SwizzleComponent::Y => 'y',
                SwizzleComponent::Z => 'z',
                SwizzleComponent::W => 'w',
            };
            // Clamp to the D3D9 constant register range to avoid WGSL OOB access.
            let idx_expr = format!(
                "u32(clamp(i32({}) + ({}.{comp}), 0, 255))",
                src.reg.index, rel_reg
            );
            let mut expr = format!("constants.c[CONST_BASE + {idx_expr}]");
            // `def c#` defines behave like constant-register writes that occur before shader
            // execution. They must override the uniform constant buffer even for relative indexing.
            //
            // Model this by selecting the embedded value when the computed constant index matches
            // a defined register.
            for def_idx in f32_defs.keys() {
                expr = format!("select({expr}, c{def_idx}, ({idx_expr} == {def_idx}u))");
            }
            expr
        } else {
            reg_var_name(&src.reg)?
        }
    } else {
        reg_var_name(&src.reg)?
    };
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

fn cond_expr(cond: &Cond, f32_defs: &BTreeMap<u32, [f32; 4]>) -> Result<String, WgslError> {
    match cond {
        Cond::NonZero { src } => {
            let (expr, ty) = src_expr(src, f32_defs)?;
            Ok(match ty {
                ScalarTy::F32 => format!("({expr}.x != 0.0)"),
                ScalarTy::I32 => format!("({expr}.x != 0)"),
                ScalarTy::Bool => format!("{expr}.x"),
            })
        }
        Cond::Compare { op, src0, src1 } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
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
        Cond::Predicate { pred } => predicate_expr(pred),
    }
}

fn predicate_expr(pred: &PredicateRef) -> Result<String, WgslError> {
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

fn emit_op_line(op: &IrOp, f32_defs: &BTreeMap<u32, [f32; 4]>) -> Result<String, WgslError> {
    match op {
        IrOp::Mov {
            dst,
            src,
            modifiers,
        } => {
            let (src_e, src_ty) = src_expr(src, f32_defs)?;
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
        IrOp::Mova {
            dst,
            src,
            modifiers,
        } => {
            // D3D9 `mova` converts float → int and stores the result in an address register (`a#`).
            //
            // Exact rounding behavior is GPU-dependent; WGSL `vec4<i32>(vec4<f32>)` conversion is a
            // deterministic truncation toward zero, which is a reasonable approximation for the
            // common case (non-negative indices).
            let (src_e, src_ty) = src_expr(src, f32_defs)?;
            if src_ty != ScalarTy::F32 {
                return Err(err("mova source must be float"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::I32 {
                return Err(err("mova destination must be integer"));
            }
            let src_e = apply_float_result_modifiers(src_e, modifiers)?;
            emit_assign(dst, format!("vec4<i32>({src_e})"))
        }
        IrOp::Add {
            dst,
            src0,
            src1,
            modifiers,
        } => emit_float_binop(dst, src0, src1, modifiers, f32_defs, "+"),
        IrOp::Sub {
            dst,
            src0,
            src1,
            modifiers,
        } => emit_float_binop(dst, src0, src1, modifiers, f32_defs, "-"),
        IrOp::Mul {
            dst,
            src0,
            src1,
            modifiers,
        } => emit_float_binop(dst, src0, src1, modifiers, f32_defs, "*"),
        IrOp::Mad {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
            let (c, cty) = src_expr(src2, f32_defs)?;
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
        IrOp::Lrp {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
            let (c, cty) = src_expr(src2, f32_defs)?;
            if aty != ScalarTy::F32 || bty != ScalarTy::F32 || cty != ScalarTy::F32 {
                return Err(err("lrp only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("lrp destination must be float"));
            }
            // D3D9 `lrp`: dst = s0*s1 + (1-s0)*s2 = mix(s2, s1, s0).
            let e = apply_float_result_modifiers(format!("mix({c}, {b}, {a})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Min {
            dst,
            src0,
            src1,
            modifiers,
        } => emit_float_func2(dst, src0, src1, modifiers, f32_defs, "min"),
        IrOp::Max {
            dst,
            src0,
            src1,
            modifiers,
        } => emit_float_func2(dst, src0, src1, modifiers, f32_defs, "max"),
        IrOp::Rcp {
            dst,
            src,
            modifiers,
        } => {
            let (s, ty) = src_expr(src, f32_defs)?;
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
        IrOp::Rsq {
            dst,
            src,
            modifiers,
        } => {
            let (s, ty) = src_expr(src, f32_defs)?;
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
        IrOp::Frc {
            dst,
            src,
            modifiers,
        } => {
            let (s, ty) = src_expr(src, f32_defs)?;
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
        IrOp::Exp { dst, src, modifiers } => {
            emit_float_func1(dst, src, modifiers, f32_defs, "exp2")
        }
        IrOp::Log { dst, src, modifiers } => {
            emit_float_func1(dst, src, modifiers, f32_defs, "log2")
        }
        IrOp::Ddx { dst, src, modifiers } => {
            let (s, ty) = src_expr(src, f32_defs)?;
            if ty != ScalarTy::F32 {
                return Err(err("dsx only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dsx destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("dpdx({s})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Ddy { dst, src, modifiers } => {
            let (s, ty) = src_expr(src, f32_defs)?;
            if ty != ScalarTy::F32 {
                return Err(err("dsy only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dsy destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("dpdy({s})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Dp2 {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
            if aty != ScalarTy::F32 || bty != ScalarTy::F32 {
                return Err(err("dp2 only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dp2 destination must be float"));
            }
            let dot = format!("dot(({a}).xy, ({b}).xy)");
            let e = apply_float_result_modifiers(format!("vec4<f32>({dot})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Dp3 {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
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
        IrOp::Dp4 {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
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
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
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
            let (cond_e, cond_ty) = src_expr(cond, f32_defs)?;
            let (a, aty) = src_expr(src_ge, f32_defs)?;
            let (b, bty) = src_expr(src_lt, f32_defs)?;
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
        IrOp::Pow {
            dst,
            src0,
            src1,
            modifiers,
        } => emit_float_func2(dst, src0, src1, modifiers, f32_defs, "pow"),
        IrOp::TexSample { .. } => Err(err("texture sampling not supported in WGSL lowering")),
    }
}

fn emit_float_func1(
    dst: &Dst,
    src: &Src,
    modifiers: &InstModifiers,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    func: &str,
) -> Result<String, WgslError> {
    let (s, ty) = src_expr(src, f32_defs)?;
    if ty != ScalarTy::F32 {
        return Err(err("float function uses non-float source"));
    }
    let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
    if dst_ty != ScalarTy::F32 {
        return Err(err("float function destination must be float"));
    }
    let e = apply_float_result_modifiers(format!("{func}({s})"), modifiers)?;
    emit_assign(dst, e)
}

fn emit_float_binop(
    dst: &Dst,
    src0: &Src,
    src1: &Src,
    modifiers: &InstModifiers,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    op: &str,
) -> Result<String, WgslError> {
    let (a, aty) = src_expr(src0, f32_defs)?;
    let (b, bty) = src_expr(src1, f32_defs)?;
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
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    func: &str,
) -> Result<String, WgslError> {
    let (a, aty) = src_expr(src0, f32_defs)?;
    let (b, bty) = src_expr(src1, f32_defs)?;
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
    // WGSL does not support assignments to multi-component swizzles (e.g. `v.xy = ...`), so
    // lower write masks to per-component assignments.
    //
    // Note: single-component assignments (`v.x = ...`) are permitted.
    if dst.mask.0 == 0 {
        // No components written.
        return Ok(String::new());
    }
    if dst.mask.0 == 0xF {
        return Ok(format!("{dst_name} = {value};"));
    }

    let mut comps = Vec::new();
    if dst.mask.contains(SwizzleComponent::X) {
        comps.push('x');
    }
    if dst.mask.contains(SwizzleComponent::Y) {
        comps.push('y');
    }
    if dst.mask.contains(SwizzleComponent::Z) {
        comps.push('z');
    }
    if dst.mask.contains(SwizzleComponent::W) {
        comps.push('w');
    }

    if comps.len() == 1 {
        let c = comps[0];
        return Ok(format!("{dst_name}.{c} = ({value}).{c};"));
    }

    // Use a block to create a nested scope so we can reuse the same temporary name for every op.
    // This keeps codegen simple without needing a global unique-name allocator.
    let mut out = String::new();
    out.push_str("{ let _tmp = ");
    out.push_str(&value);
    out.push_str("; ");
    for c in comps {
        out.push_str(&format!("{dst_name}.{c} = _tmp.{c}; "));
    }
    out.push('}');
    Ok(out)
}

fn emit_block(
    wgsl: &mut String,
    block: &Block,
    indent: usize,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
) -> Result<(), WgslError> {
    for stmt in &block.stmts {
        emit_stmt(wgsl, stmt, indent, f32_defs)?;
    }
    Ok(())
}

fn emit_stmt(
    wgsl: &mut String,
    stmt: &Stmt,
    indent: usize,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
) -> Result<(), WgslError> {
    let pad = "  ".repeat(indent);
    match stmt {
        Stmt::Op(op) => {
            if let Some(pred) = &op_modifiers(op).predicate {
                let pred_cond = predicate_expr(pred)?;
                let _ = writeln!(wgsl, "{pad}if ({pred_cond}) {{");
                let line = emit_op_line(op, f32_defs)?;
                let inner_pad = "  ".repeat(indent + 1);
                let _ = writeln!(wgsl, "{inner_pad}{line}");
                let _ = writeln!(wgsl, "{pad}}}");
            } else {
                let line = emit_op_line(op, f32_defs)?;
                let _ = writeln!(wgsl, "{pad}{line}");
            }
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
        } => {
            let cond = cond_expr(cond, f32_defs)?;
            let _ = writeln!(wgsl, "{pad}if ({cond}) {{");
            emit_block(wgsl, then_block, indent + 1, f32_defs)?;
            if let Some(else_block) = else_block {
                let _ = writeln!(wgsl, "{pad}}} else {{");
                emit_block(wgsl, else_block, indent + 1, f32_defs)?;
            }
            let _ = writeln!(wgsl, "{pad}}}");
        }
        Stmt::Loop { body } => {
            let _ = writeln!(wgsl, "{pad}loop {{");
            emit_block(wgsl, body, indent + 1, f32_defs)?;
            let _ = writeln!(wgsl, "{pad}}}");
        }
        Stmt::Break => {
            let _ = writeln!(wgsl, "{pad}break;");
        }
        Stmt::BreakIf { cond } => {
            let cond = cond_expr(cond, f32_defs)?;
            let _ = writeln!(wgsl, "{pad}if ({cond}) {{ break; }}");
        }
        Stmt::Discard { src } => {
            // D3D9 texkill: discard the pixel if any component of the source is < 0.
            //
            // The source operand swizzle and modifier are already applied by `src_expr`.
            let (src_e, src_ty) = src_expr(src)?;
            if src_ty != ScalarTy::F32 {
                return Err(err("texkill requires a float source"));
            }

            let _ = writeln!(wgsl, "{pad}if (any(({src_e}) < vec4<f32>(0.0))) {{");
            let inner_pad = "  ".repeat(indent + 1);
            let _ = writeln!(wgsl, "{inner_pad}discard;");
            let _ = writeln!(wgsl, "{pad}}}");
        }
    }
    Ok(())
}

fn op_modifiers(op: &IrOp) -> &InstModifiers {
    match op {
        IrOp::Mov { modifiers, .. }
        | IrOp::Mova { modifiers, .. }
        | IrOp::Add { modifiers, .. }
        | IrOp::Sub { modifiers, .. }
        | IrOp::Mul { modifiers, .. }
        | IrOp::Mad { modifiers, .. }
        | IrOp::Lrp { modifiers, .. }
        | IrOp::Dp2 { modifiers, .. }
        | IrOp::Dp3 { modifiers, .. }
        | IrOp::Dp4 { modifiers, .. }
        | IrOp::Rcp { modifiers, .. }
        | IrOp::Rsq { modifiers, .. }
        | IrOp::Frc { modifiers, .. }
        | IrOp::Exp { modifiers, .. }
        | IrOp::Log { modifiers, .. }
        | IrOp::Ddx { modifiers, .. }
        | IrOp::Ddy { modifiers, .. }
        | IrOp::Min { modifiers, .. }
        | IrOp::Max { modifiers, .. }
        | IrOp::SetCmp { modifiers, .. }
        | IrOp::Select { modifiers, .. }
        | IrOp::Pow { modifiers, .. }
        | IrOp::TexSample { modifiers, .. } => modifiers,
    }
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

    let entry_point = match ir.version.stage {
        ShaderStage::Vertex => "vs_main",
        ShaderStage::Pixel => "fs_main",
    };

    // Semantic lookup tables, keyed by (RegFile, index).
    let mut input_semantics: BTreeMap<(RegFile, u32), Semantic> = BTreeMap::new();
    for decl in &ir.inputs {
        if decl.reg.relative.is_some() {
            continue;
        }
        input_semantics.insert((decl.reg.file, decl.reg.index), decl.semantic.clone());
    }

    let mut output_semantics: BTreeMap<(RegFile, u32), Semantic> = BTreeMap::new();
    for decl in &ir.outputs {
        if decl.reg.relative.is_some() {
            continue;
        }
        output_semantics.insert((decl.reg.file, decl.reg.index), decl.semantic.clone());
    }

    let mut wgsl = String::new();

    // Float constants: pack per-stage `c#` register files into a single uniform buffer to keep
    // bindings stable across shader stages (VS=0..255, PS=256..511).
    wgsl.push_str("struct Constants { c: array<vec4<f32>, 512>, };\n");
    wgsl.push_str("@group(0) @binding(0) var<uniform> constants: Constants;\n");
    let const_base = match ir.version.stage {
        ShaderStage::Vertex => 0u32,
        ShaderStage::Pixel => 256u32,
    };
    let _ = writeln!(wgsl, "const CONST_BASE: u32 = {}u;\n", const_base);

    let emit_const_decls = |wgsl: &mut String| {
        // Embedded float constants (`def c#`). These behave like constant-register writes that occur
        // before shader execution and must override the uniform constant buffer even when accessed
        // via relative indexing (`cN[a0.x]`).
        for (idx, value) in &f32_defs {
            let _ = writeln!(
                wgsl,
                "  let c{idx}: vec4<f32> = vec4<f32>({}, {}, {}, {});",
                format_f32(value[0]),
                format_f32(value[1]),
                format_f32(value[2]),
                format_f32(value[3])
            );
        }
        // Non-embedded float constants come from the uniform constant buffer.
        for idx in &usage.float_consts {
            if f32_defs.contains_key(idx) {
                continue;
            }
            let _ = writeln!(
                wgsl,
                "  let c{idx}: vec4<f32> = constants.c[CONST_BASE + {idx}u];"
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
    };

    match ir.version.stage {
        ShaderStage::Vertex => {
            // Vertex attributes (`v#`).
            let mut vs_inputs = BTreeSet::<u32>::new();
            for (file, index) in &usage.inputs {
                if *file == RegFile::Input {
                    vs_inputs.insert(*index);
                }
            }
            let has_inputs = !vs_inputs.is_empty();
            if has_inputs && !ir.uses_semantic_locations {
                // Without semantic-based remapping we treat v# indices as WGSL locations directly.
                // Stay within WebGPU's guaranteed minimum to avoid generating shaders that won't
                // validate on some devices.
                if let Some(&max_v) = vs_inputs.iter().max() {
                    if max_v >= WEBGPU_MIN_VERTEX_ATTRIBUTES {
                        return Err(err(format!(
                            "vertex shader uses input v{max_v} but semantic-based location mapping is unavailable; refusing to emit WGSL with @location({max_v}) (WebGPU guaranteed min maxVertexAttributes is {WEBGPU_MIN_VERTEX_ATTRIBUTES})"
                        )));
                    }
                }
            }

            // Inter-stage varyings written by the vertex shader.
            let mut vs_varyings = BTreeSet::<(RegFile, u32)>::new();
            for decl in &ir.outputs {
                if decl.reg.relative.is_some() {
                    continue;
                }
                if matches!(
                    decl.reg.file,
                    RegFile::AttrOut | RegFile::TexCoordOut | RegFile::Output
                ) {
                    vs_varyings.insert((decl.reg.file, decl.reg.index));
                }
            }
            for (file, index) in &usage.outputs {
                if matches!(*file, RegFile::AttrOut | RegFile::TexCoordOut | RegFile::Output) {
                    vs_varyings.insert((*file, *index));
                }
            }

            // Assign `@location` values and check for collisions.
            let mut vs_varying_locations: BTreeMap<(RegFile, u32), u32> = BTreeMap::new();
            let mut loc_to_reg: BTreeMap<u32, (RegFile, u32)> = BTreeMap::new();
            for (file, index) in &vs_varyings {
                let semantic = output_semantics.get(&(*file, *index));
                let loc = varying_location(*file, *index, semantic)?;
                if let Some((prev_file, prev_index)) = loc_to_reg.insert(loc, (*file, *index)) {
                    return Err(err(format!(
                        "multiple vertex outputs map to @location({loc}): {prev_file:?}{prev_index} and {file:?}{index}"
                    )));
                }
                vs_varying_locations.insert((*file, *index), loc);
            }

            if has_inputs {
                wgsl.push_str("struct VsInput {\n");
                for idx in &vs_inputs {
                    // Register indices are already canonicalized via semantic remapping in the IR builder.
                    let _ = writeln!(wgsl, "  @location({idx}) v{idx}: vec4<f32>,");
                }
                wgsl.push_str("};\n\n");
            }

            wgsl.push_str("struct VsOut {\n  @builtin(position) pos: vec4<f32>,\n");
            for ((file, index), loc) in &vs_varying_locations {
                let reg = RegRef {
                    file: *file,
                    index: *index,
                    relative: None,
                };
                let name = reg_var_name(&reg)?;
                let _ = writeln!(wgsl, "  @location({loc}) {name}: vec4<f32>,");
            }
            wgsl.push_str("};\n\n");

            if has_inputs {
                wgsl.push_str("@vertex\nfn vs_main(input: VsInput) -> VsOut {\n");
            } else {
                wgsl.push_str("@vertex\nfn vs_main() -> VsOut {\n");
            }

            // Local temp registers.
            for r in &usage.temps {
                let _ = writeln!(wgsl, "  var r{r}: vec4<f32> = vec4<f32>(0.0);");
            }

            // Address registers (`a#`) used for relative constant indexing.
            for a in &usage.addrs {
                let _ = writeln!(wgsl, "  var a{a}: vec4<i32> = vec4<i32>(0);");
            }

            // Predicate registers (`p#`).
            for p in &usage.predicates {
                let _ = writeln!(wgsl, "  var p{p}: vec4<bool> = vec4<bool>(false);");
            }

            // Bind vertex inputs to locals that match the D3D register naming (`v#`).
            if has_inputs {
                for idx in &vs_inputs {
                    let _ = writeln!(wgsl, "  let v{idx}: vec4<f32> = input.v{idx};");
                }
            }

            // Outputs used by the shader. These are mutable locals that get copied into the return
            // value at the end.
            let mut required_outputs = usage.outputs.clone();
            // Always provide `oPos` so we can emit a stable return struct.
            required_outputs.insert((RegFile::RastOut, 0));
            // Ensure all varyings in the interface are declared, even if never written.
            required_outputs.extend(vs_varyings.iter().copied());

            for (file, index) in &required_outputs {
                let reg = RegRef {
                    file: *file,
                    index: *index,
                    relative: None,
                };
                let ty = reg_scalar_ty(*file).unwrap_or(ScalarTy::F32);
                let name = reg_var_name(&reg)?;
                let _ = writeln!(
                    wgsl,
                    "  var {name}: {} = {};",
                    ty.wgsl_vec4(),
                    default_vec4(ty)
                );
            }

            emit_const_decls(&mut wgsl);

            wgsl.push('\n');
            emit_block(&mut wgsl, &ir.body, 1, &f32_defs)?;

            wgsl.push_str("  var out: VsOut;\n");
            wgsl.push_str("  out.pos = oPos;\n");
            for ((file, index), _) in &vs_varying_locations {
                let reg = RegRef {
                    file: *file,
                    index: *index,
                    relative: None,
                };
                let name = reg_var_name(&reg)?;
                let _ = writeln!(wgsl, "  out.{name} = {name};");
            }
            wgsl.push_str("  return out;\n}\n");
        }
        ShaderStage::Pixel => {
            // Inter-stage varyings read by the pixel shader.
            let mut ps_inputs = BTreeSet::<(RegFile, u32)>::new();
            for (file, index) in &usage.inputs {
                if matches!(*file, RegFile::Input | RegFile::Texture) {
                    ps_inputs.insert((*file, *index));
                }
            }
            let has_inputs = !ps_inputs.is_empty();

            let mut ps_input_locations: BTreeMap<(RegFile, u32), u32> = BTreeMap::new();
            let mut loc_to_reg: BTreeMap<u32, (RegFile, u32)> = BTreeMap::new();
            for (file, index) in &ps_inputs {
                let semantic = input_semantics.get(&(*file, *index));
                let loc = varying_location(*file, *index, semantic)?;
                if let Some((prev_file, prev_index)) = loc_to_reg.insert(loc, (*file, *index)) {
                    return Err(err(format!(
                        "multiple pixel shader inputs map to @location({loc}): {prev_file:?}{prev_index} and {file:?}{index}"
                    )));
                }
                ps_input_locations.insert((*file, *index), loc);
            }

            // D3D9 pixel shaders conceptually write at least oC0. Keep the generated WGSL stable by
            // always emitting location(0), even if the shader bytecode never assigns it.
            let mut color_outputs = BTreeSet::<u32>::new();
            for (file, index) in &usage.outputs {
                if *file == RegFile::ColorOut {
                    color_outputs.insert(*index);
                }
            }
            color_outputs.insert(0);

            wgsl.push_str("struct FsOut {\n");
            for idx in &color_outputs {
                let _ = writeln!(wgsl, "  @location({idx}) oC{idx}: vec4<f32>,");
            }
            wgsl.push_str("};\n\n");

            if has_inputs {
                wgsl.push_str("struct FsIn {\n");
                for ((file, index), loc) in &ps_input_locations {
                    let reg = RegRef {
                        file: *file,
                        index: *index,
                        relative: None,
                    };
                    let name = reg_var_name(&reg)?;
                    let _ = writeln!(wgsl, "  @location({loc}) {name}: vec4<f32>,");
                }
                wgsl.push_str("};\n\n");
                wgsl.push_str("@fragment\nfn fs_main(input: FsIn) -> FsOut {\n");
            } else {
                // WGSL does not permit empty structs, so if the shader uses no varyings we omit the
                // input parameter entirely.
                wgsl.push_str("@fragment\nfn fs_main() -> FsOut {\n");
            }

            // Local temp registers.
            for r in &usage.temps {
                let _ = writeln!(wgsl, "  var r{r}: vec4<f32> = vec4<f32>(0.0);");
            }

            // Address registers (`a#`) used for relative constant indexing.
            for a in &usage.addrs {
                let _ = writeln!(wgsl, "  var a{a}: vec4<i32> = vec4<i32>(0);");
            }

            // Predicate registers (`p#`).
            for p in &usage.predicates {
                let _ = writeln!(wgsl, "  var p{p}: vec4<bool> = vec4<bool>(false);");
            }

            // Bind pixel inputs to locals that match the D3D register naming (`v#` / `t#`).
            if has_inputs {
                for (file, index) in &ps_inputs {
                    let reg = RegRef {
                        file: *file,
                        index: *index,
                        relative: None,
                    };
                    let name = reg_var_name(&reg)?;
                    let _ = writeln!(wgsl, "  let {name}: vec4<f32> = input.{name};");
                }
            }

            // Outputs used by the shader. These are mutable locals that get copied into the return
            // value at the end.
            let mut required_outputs = usage.outputs.clone();
            required_outputs.extend(color_outputs.iter().map(|&idx| (RegFile::ColorOut, idx)));

            for (file, index) in &required_outputs {
                let reg = RegRef {
                    file: *file,
                    index: *index,
                    relative: None,
                };
                let ty = reg_scalar_ty(*file).unwrap_or(ScalarTy::F32);
                let name = reg_var_name(&reg)?;
                let _ = writeln!(
                    wgsl,
                    "  var {name}: {} = {};",
                    ty.wgsl_vec4(),
                    default_vec4(ty)
                );
            }

            emit_const_decls(&mut wgsl);

            wgsl.push('\n');
            emit_block(&mut wgsl, &ir.body, 1, &f32_defs)?;

            wgsl.push_str("  var out: FsOut;\n");
            for idx in &color_outputs {
                let _ = writeln!(wgsl, "  out.oC{idx} = oC{idx};");
            }
            wgsl.push_str("  return out;\n}\n");
        }
    }

    Ok(WgslOutput { wgsl, entry_point })
}
