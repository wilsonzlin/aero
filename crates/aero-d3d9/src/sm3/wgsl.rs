use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

use crate::shader_limits::{
    MAX_D3D9_SAMPLER_REGISTER_INDEX, MAX_D3D9_SHADER_CONTROL_FLOW_NESTING,
    MAX_D3D9_SHADER_REGISTER_INDEX, MAX_D3D9_WGSL_BYTES,
};
use crate::sm3::decode::{ResultShift, SrcModifier, Swizzle, SwizzleComponent, TextureType};
use crate::sm3::ir::{
    Block, CompareOp, Cond, Dst, InstModifiers, IrOp, PredicateRef, RegFile, RegRef, Semantic, Src,
    Stmt,
};
use crate::sm3::types::{ShaderStage, ShaderVersion};

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
    pub bind_group_layout: BindGroupLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindGroupLayout {
    /// Bind group index used for texture/sampler bindings in this shader stage.
    ///
    /// Contract:
    /// - group(0): constants shared by VS/PS (binding 0, see `Constants` in WGSL output)
    /// - group(1): VS texture/sampler bindings
    /// - group(2): PS texture/sampler bindings
    /// - group(3): optional half-pixel-center uniform buffer (VS only)
    pub sampler_group: u32,
    /// sampler_index -> (texture_binding, sampler_binding)
    pub sampler_bindings: HashMap<u32, (u32, u32)>,
    /// sampler_index -> texture type (from `dcl_* s#`, defaulting to 2D when absent).
    pub sampler_texture_types: HashMap<u32, TextureType>,
}

fn sampler_bind_group(stage: ShaderStage) -> u32 {
    match stage {
        ShaderStage::Vertex => 1,
        ShaderStage::Pixel => 2,
    }
}

/// Output of SM2/SM3 bytecode → WGSL translation.
#[derive(Debug, Clone)]
pub struct WgslTranslation {
    pub version: ShaderVersion,
    pub wgsl: String,
    pub entry_point: &'static str,
    pub bind_group_layout: BindGroupLayout,
}

#[derive(Debug, thiserror::Error)]
pub enum Sm3WgslError {
    #[error(transparent)]
    Decode(#[from] crate::sm3::decode::DecodeError),
    #[error(transparent)]
    Build(#[from] crate::sm3::ir_builder::BuildError),
    #[error(transparent)]
    Verify(#[from] crate::sm3::verify::VerifyError),
    #[error(transparent)]
    Wgsl(#[from] WgslError),
}

/// Translates a raw D3D9 SM2/SM3 token stream (DWORD bytecode) to WGSL.
///
/// The input must be the legacy D3D9 token stream itself (i.e. the `SHDR`/`SHEX` payload), not a
/// DXBC container.
///
/// For translation options that affect WGSL semantics (e.g. D3D9 half-pixel center adjustment),
/// use [`translate_to_wgsl_with_options`].
pub fn translate_to_wgsl(token_stream: &[u8]) -> Result<WgslTranslation, Sm3WgslError> {
    translate_to_wgsl_with_options(token_stream, WgslOptions::default())
}

pub fn translate_to_wgsl_with_options(
    token_stream: &[u8],
    options: WgslOptions,
) -> Result<WgslTranslation, Sm3WgslError> {
    let token_stream = crate::token_stream::normalize_sm2_sm3_instruction_lengths(token_stream)
        .map_err(|message| crate::sm3::decode::DecodeError {
            token_index: 0,
            message,
        })?;
    let decoded = crate::sm3::decode_u8_le_bytes(token_stream.as_ref())?;
    let ir = crate::sm3::build_ir(&decoded)?;
    crate::sm3::verify_ir(&ir)?;
    let WgslOutput {
        wgsl,
        entry_point,
        bind_group_layout,
    } = generate_wgsl_with_options(&ir, options)?;
    Ok(WgslTranslation {
        version: ir.version,
        wgsl,
        entry_point,
        bind_group_layout,
    })
}

/// Options that affect the emitted WGSL *semantics*.
///
/// These must participate in any shader cache key derivation because toggling them changes the
/// generated WGSL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
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

fn wgsl_texture_type(ty: TextureType) -> Result<&'static str, WgslError> {
    Ok(match ty {
        // D3D9 "1D" samplers are backed by ordinary 2D textures with height=1. Our D3D9 executor
        // binds textures as 2D, so lower 1D samplers to `texture_2d` bindings and fix up sample
        // coordinates during WGSL emission.
        TextureType::Texture1D => "texture_2d<f32>",
        TextureType::Texture2D => "texture_2d<f32>",
        TextureType::Texture3D => "texture_3d<f32>",
        TextureType::TextureCube => "texture_cube<f32>",
        TextureType::Unknown(v) => {
            return Err(err(format!(
                "unsupported sampler texture type: Unknown({v})"
            )));
        }
    })
}

fn tex_coord_swizzle(ty: TextureType) -> Result<&'static str, WgslError> {
    Ok(match ty {
        TextureType::Texture1D => "x",
        TextureType::Texture2D => "xy",
        TextureType::Texture3D | TextureType::TextureCube => "xyz",
        TextureType::Unknown(v) => {
            return Err(err(format!(
                "unsupported sampler texture type: Unknown({v})"
            )));
        }
    })
}

fn tex_coord_expr(coord_e: &str, ty: TextureType, project: bool) -> Result<String, WgslError> {
    let swz = tex_coord_swizzle(ty)?;
    if ty == TextureType::Texture1D {
        // See `wgsl_texture_type`: treat 1D samplers as 2D bindings (height=1) and keep the Y
        // coordinate constant so the sample result is independent of the source operand's Y
        // component.
        let x = if project {
            format!("(({coord_e}).{swz} / ({coord_e}).w)")
        } else {
            format!("({coord_e}).{swz}")
        };
        return Ok(format!("vec2<f32>({x}, 0.5)"));
    }
    Ok(if project {
        format!("(({coord_e}).{swz} / ({coord_e}).w)")
    } else {
        format!("({coord_e}).{swz}")
    })
}

fn tex_grad_expr(grad_e: &str, ty: TextureType) -> Result<String, WgslError> {
    let swz = tex_coord_swizzle(ty)?;
    if ty == TextureType::Texture1D {
        return Ok(format!("vec2<f32>(({grad_e}).{swz}, 0.0)"));
    }
    Ok(format!("({grad_e}).{swz}"))
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
pub(crate) fn varying_location(
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
            None => 4 + index,
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
    loop_regs: BTreeSet<u32>,
    inputs: BTreeSet<(RegFile, u32)>,
    misc_inputs: BTreeSet<u32>,
    outputs_used: BTreeSet<(RegFile, u32)>,
    outputs_written: BTreeSet<(RegFile, u32)>,
    samplers: BTreeSet<u32>,
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
            loop_regs: BTreeSet::new(),
            inputs: BTreeSet::new(),
            misc_inputs: BTreeSet::new(),
            outputs_used: BTreeSet::new(),
            outputs_written: BTreeSet::new(),
            samplers: BTreeSet::new(),
            predicates: BTreeSet::new(),
            float_consts: BTreeSet::new(),
            int_consts: BTreeSet::new(),
            bool_consts: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegAccess {
    Read,
    Write,
}

fn collect_reg_usage(block: &Block, usage: &mut RegUsage, depth: usize) -> Result<(), WgslError> {
    if depth > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        return Err(err(format!(
            "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
        )));
    }
    for stmt in &block.stmts {
        match stmt {
            Stmt::Op(op) => collect_op_usage(op, usage),
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                collect_cond_usage(cond, usage);
                collect_reg_usage(then_block, usage, depth + 1)?;
                if let Some(else_block) = else_block {
                    collect_reg_usage(else_block, usage, depth + 1)?;
                }
            }
            Stmt::Loop { init, body } => {
                collect_reg_ref_usage(&init.loop_reg, usage, RegAccess::Read);
                collect_reg_ref_usage(&init.ctrl_reg, usage, RegAccess::Read);
                collect_reg_usage(body, usage, depth + 1)?;
            }
            Stmt::Rep { count_reg, body } => {
                // `rep` implicitly uses the `aL` loop register as the counter.
                let loop_reg = RegRef {
                    file: RegFile::Loop,
                    index: 0,
                    relative: None,
                };
                collect_reg_ref_usage(&loop_reg, usage, RegAccess::Write);
                collect_reg_ref_usage(count_reg, usage, RegAccess::Read);
                collect_reg_usage(body, usage, depth + 1)?;
            }
            Stmt::Break => {}
            Stmt::BreakIf { cond } => collect_cond_usage(cond, usage),
            Stmt::Discard { src } => collect_src_usage(src, usage),
            Stmt::Call { .. } | Stmt::Return => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct SubroutineInfo {
    /// True if this subroutine (transitively) contains WGSL ops that require uniform control flow
    /// in fragment stage (`dpdx`/`dpdy`/`textureSample`/`textureSampleBias`).
    uses_derivatives: bool,
    /// True if this subroutine (transitively) can discard the fragment (`texkill`/`discard`).
    ///
    /// Discard is not safely "rollback-able", so we avoid speculative execution for these
    /// subroutines.
    may_discard: bool,
    /// Registers written by this subroutine (transitively).
    writes: BTreeSet<(RegFile, u32)>,
}

fn op_dst(op: &IrOp) -> &Dst {
    match op {
        IrOp::Mov { dst, .. }
        | IrOp::Mova { dst, .. }
        | IrOp::Add { dst, .. }
        | IrOp::Sub { dst, .. }
        | IrOp::Mul { dst, .. }
        | IrOp::Mad { dst, .. }
        | IrOp::Lrp { dst, .. }
        | IrOp::Dp2 { dst, .. }
        | IrOp::Dp2Add { dst, .. }
        | IrOp::Dp3 { dst, .. }
        | IrOp::Dp4 { dst, .. }
        | IrOp::MatrixMul { dst, .. }
        | IrOp::Dst { dst, .. }
        | IrOp::Crs { dst, .. }
        | IrOp::Rcp { dst, .. }
        | IrOp::Rsq { dst, .. }
        | IrOp::Frc { dst, .. }
        | IrOp::Abs { dst, .. }
        | IrOp::Sgn { dst, .. }
        | IrOp::Exp { dst, .. }
        | IrOp::Log { dst, .. }
        | IrOp::Ddx { dst, .. }
        | IrOp::Ddy { dst, .. }
        | IrOp::Nrm { dst, .. }
        | IrOp::Lit { dst, .. }
        | IrOp::SinCos { dst, .. }
        | IrOp::Min { dst, .. }
        | IrOp::Max { dst, .. }
        | IrOp::SetCmp { dst, .. }
        | IrOp::Select { dst, .. }
        | IrOp::Pow { dst, .. }
        | IrOp::TexSample { dst, .. } => dst,
    }
}

fn op_uses_derivatives(op: &IrOp, stage: ShaderStage) -> bool {
    match op {
        IrOp::Ddx { .. } | IrOp::Ddy { .. } => true,
        IrOp::TexSample { kind, .. } if stage == ShaderStage::Pixel => matches!(
            kind,
            crate::sm3::ir::TexSampleKind::ImplicitLod { .. } | crate::sm3::ir::TexSampleKind::Bias
        ),
        _ => false,
    }
}

fn collect_subroutine_info_direct(
    block: &Block,
    stage: ShaderStage,
    info: &mut SubroutineInfo,
    called_labels: &mut BTreeSet<u32>,
    depth: usize,
) -> Result<(), WgslError> {
    if depth > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        return Err(err(format!(
            "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
        )));
    }
    for stmt in &block.stmts {
        match stmt {
            Stmt::Op(op) => {
                if op_uses_derivatives(op, stage) {
                    info.uses_derivatives = true;
                }
                let dst = op_dst(op);
                if dst.reg.relative.is_some() {
                    return Err(err(
                        "relative register addressing is not supported in subroutine write analysis",
                    ));
                }
                info.writes.insert((dst.reg.file, dst.reg.index));
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                collect_subroutine_info_direct(then_block, stage, info, called_labels, depth + 1)?;
                if let Some(else_block) = else_block {
                    collect_subroutine_info_direct(
                        else_block,
                        stage,
                        info,
                        called_labels,
                        depth + 1,
                    )?;
                }
            }
            Stmt::Loop { body, .. } | Stmt::Rep { body, .. } => {
                collect_subroutine_info_direct(body, stage, info, called_labels, depth + 1)?;
            }
            Stmt::Break | Stmt::BreakIf { .. } | Stmt::Return => {}
            Stmt::Discard { .. } => info.may_discard = true,
            Stmt::Call { label } => {
                called_labels.insert(*label);
            }
        }
    }
    Ok(())
}

fn build_subroutine_info_map(
    ir: &crate::sm3::ir::ShaderIr,
) -> Result<HashMap<u32, SubroutineInfo>, WgslError> {
    fn dfs(
        label: u32,
        ir: &crate::sm3::ir::ShaderIr,
        out: &mut HashMap<u32, SubroutineInfo>,
        visiting: &mut BTreeSet<u32>,
    ) -> Result<SubroutineInfo, WgslError> {
        if let Some(info) = out.get(&label) {
            return Ok(info.clone());
        }
        if !visiting.insert(label) {
            return Err(err(format!(
                "recursive subroutine call detected in WGSL lowering (l{label})"
            )));
        }

        let body = ir
            .subroutines
            .get(&label)
            .ok_or_else(|| err(format!("call target label l{label} is not defined")))?;

        let mut info = SubroutineInfo::default();
        let mut called = BTreeSet::<u32>::new();
        collect_subroutine_info_direct(body, ir.version.stage, &mut info, &mut called, 0)?;

        for callee in called {
            let child = dfs(callee, ir, out, visiting)?;
            info.uses_derivatives |= child.uses_derivatives;
            info.may_discard |= child.may_discard;
            info.writes.extend(child.writes);
        }

        visiting.remove(&label);
        out.insert(label, info.clone());
        Ok(info)
    }

    let mut out: HashMap<u32, SubroutineInfo> = HashMap::new();
    let mut visiting = BTreeSet::<u32>::new();
    for &label in ir.subroutines.keys() {
        let _ = dfs(label, ir, &mut out, &mut visiting)?;
    }
    Ok(out)
}

fn collect_op_usage(op: &IrOp, usage: &mut RegUsage) {
    // Predicate modifier usage.
    if let Some(pred) = &op_modifiers(op).predicate {
        collect_reg_ref_usage(&pred.reg, usage, RegAccess::Read);
    }

    match op {
        IrOp::Mov {
            dst,
            src,
            modifiers,
        }
        | IrOp::Mova {
            dst,
            src,
            modifiers,
        }
        | IrOp::Rcp {
            dst,
            src,
            modifiers,
        }
        | IrOp::Rsq {
            dst,
            src,
            modifiers,
        }
        | IrOp::Frc {
            dst,
            src,
            modifiers,
        }
        | IrOp::Abs {
            dst,
            src,
            modifiers,
        }
        | IrOp::Sgn {
            dst,
            src,
            modifiers,
        }
        | IrOp::Exp {
            dst,
            src,
            modifiers,
        }
        | IrOp::Log {
            dst,
            src,
            modifiers,
        }
        | IrOp::Ddx {
            dst,
            src,
            modifiers,
        }
        | IrOp::Ddy {
            dst,
            src,
            modifiers,
        }
        | IrOp::Nrm {
            dst,
            src,
            modifiers,
        }
        | IrOp::Lit {
            dst,
            src,
            modifiers,
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src, usage);
            collect_mods_usage(modifiers, usage);
        }
        IrOp::Add {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Sub {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Mul {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Min {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Max {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp2 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp3 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp4 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dst {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Crs {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::SetCmp {
            dst,
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Pow {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
            collect_mods_usage(modifiers, usage);
        }
        IrOp::Dp2Add {
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
        IrOp::MatrixMul {
            dst,
            src0,
            src1,
            n,
            modifiers,
            ..
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src0, usage);
            // Matrix helper ops implicitly read `src1 + column_index` for 0..n.
            for col in 0..*n {
                let mut column = src1.clone();
                if let Some(idx) = column.reg.index.checked_add(u32::from(col)) {
                    column.reg.index = idx;
                }
                collect_src_usage(&column, usage);
            }
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
        IrOp::SinCos {
            dst,
            src,
            src1,
            src2,
            modifiers,
        } => {
            collect_dst_usage(dst, usage);
            collect_src_usage(src, usage);
            if let Some(src1) = src1 {
                collect_src_usage(src1, usage);
            }
            if let Some(src2) = src2 {
                collect_src_usage(src2, usage);
            }
            collect_mods_usage(modifiers, usage);
        }
        IrOp::TexSample {
            dst,
            coord,
            ddx,
            ddy,
            sampler,
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
            usage.samplers.insert(*sampler);
        }
    }
}

fn collect_mods_usage(mods: &InstModifiers, usage: &mut RegUsage) {
    if let Some(pred) = &mods.predicate {
        collect_reg_ref_usage(&pred.reg, usage, RegAccess::Read);
    }
}

fn collect_cond_usage(cond: &Cond, usage: &mut RegUsage) {
    match cond {
        Cond::NonZero { src } => collect_src_usage(src, usage),
        Cond::Compare { src0, src1, .. } => {
            collect_src_usage(src0, usage);
            collect_src_usage(src1, usage);
        }
        Cond::Predicate { pred } => collect_reg_ref_usage(&pred.reg, usage, RegAccess::Read),
    }
}

fn collect_dst_usage(dst: &Dst, usage: &mut RegUsage) {
    collect_reg_ref_usage(&dst.reg, usage, RegAccess::Write);
}

fn collect_src_usage(src: &Src, usage: &mut RegUsage) {
    collect_reg_ref_usage(&src.reg, usage, RegAccess::Read);
}

fn collect_reg_ref_usage(reg: &RegRef, usage: &mut RegUsage, access: RegAccess) {
    // Treat relative-addressing registers as an untrusted linked list: decode rejects nested
    // relative addressing for shader bytecode, but other IR construction paths (tests, future
    // features) could create arbitrarily deep chains. Use an explicit loop to avoid recursion.
    let mut current = reg;
    let mut current_access = access;
    loop {
        match current.file {
            RegFile::Temp => {
                usage.temps.insert(current.index);
            }
            RegFile::Addr => {
                usage.addrs.insert(current.index);
            }
            RegFile::Loop => {
                usage.loop_regs.insert(current.index);
            }
            RegFile::Input | RegFile::Texture => {
                usage.inputs.insert((current.file, current.index));
            }
            RegFile::MiscType => {
                usage.misc_inputs.insert(current.index);
            }
            RegFile::Sampler => {
                usage.samplers.insert(current.index);
            }
            RegFile::Predicate => {
                usage.predicates.insert(current.index);
            }
            RegFile::ColorOut
            | RegFile::DepthOut
            | RegFile::RastOut
            | RegFile::AttrOut
            | RegFile::TexCoordOut
            | RegFile::Output => {
                usage.outputs_used.insert((current.file, current.index));
                if current_access == RegAccess::Write {
                    usage.outputs_written.insert((current.file, current.index));
                }
            }
            RegFile::Const => {
                usage.float_consts.insert(current.index);
            }
            RegFile::ConstInt => {
                usage.int_consts.insert(current.index);
            }
            RegFile::ConstBool => {
                usage.bool_consts.insert(current.index);
            }
            _ => {
                // Other register files are either not represented in WGSL lowering yet
                // or are declared opportunistically when needed (e.g. inputs).
            }
        }
        let Some(rel) = &current.relative else {
            break;
        };
        current = &rel.reg;
        current_access = RegAccess::Read;
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
            match rel.reg.file {
                RegFile::Addr | RegFile::Loop => {}
                _ => {
                    return Err(err(
                        "relative constant addressing requires an address or loop register",
                    ))
                }
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
            // Embedded `def c#` constants must override the uniform constant buffer even for
            // relative indexing (`cN[a0.x]`). Naively encoding this as a nested `select(...)` chain
            // per access can cause WGSL output to balloon to enormous sizes for shaders that
            // combine many `def`s with heavy relative constant addressing.
            //
            // Instead, when there are embedded constant defs we route the lookup through a helper
            // function that performs the override selection once.
            if f32_defs.is_empty() {
                format!("constants.c[CONST_BASE + {idx_expr}]")
            } else {
                format!("aero_read_const({idx_expr})")
            }
        } else {
            // For non-relative constant reads, access the uniform buffer directly so that SM3
            // subroutine helper functions can reference `c#` registers without relying on
            // function-local `let c# = ...` bindings.
            if f32_defs.contains_key(&src.reg.index) {
                format!("c{}", src.reg.index)
            } else {
                format!("constants.c[CONST_BASE + {}u]", src.reg.index)
            }
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

fn is_uniformity_sensitive_op(op: &IrOp, stage: ShaderStage) -> bool {
    if stage != ShaderStage::Pixel {
        return false;
    }
    matches!(
        op,
        IrOp::Ddx { .. }
            | IrOp::Ddy { .. }
            | IrOp::TexSample {
                kind: crate::sm3::ir::TexSampleKind::ImplicitLod { .. }
                    | crate::sm3::ir::TexSampleKind::Bias,
                ..
            }
    )
}

fn block_contains_uniformity_sensitive_ops(
    block: &Block,
    stage: ShaderStage,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
) -> bool {
    block.stmts.iter().any(|stmt| match stmt {
        Stmt::Op(op) => is_uniformity_sensitive_op(op, stage),
        Stmt::Call { label } => subroutine_infos
            .get(label)
            .is_some_and(|info| stage == ShaderStage::Pixel && info.uses_derivatives),
        Stmt::If {
            then_block,
            else_block,
            ..
        } => {
            block_contains_uniformity_sensitive_ops(then_block, stage, subroutine_infos)
                || else_block.as_ref().is_some_and(|b| {
                    block_contains_uniformity_sensitive_ops(b, stage, subroutine_infos)
                })
        }
        Stmt::Loop { body, .. } | Stmt::Rep { body, .. } => {
            block_contains_uniformity_sensitive_ops(body, stage, subroutine_infos)
        }
        _ => false,
    })
}

fn emit_branchless_predicated_op_line(
    op: &IrOp,
    cond: &str,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
) -> Result<Option<String>, WgslError> {
    Ok(match op {
        IrOp::Ddx {
            dst,
            src,
            modifiers,
        } => {
            if stage != ShaderStage::Pixel {
                return Err(err("dsx is only supported in pixel shaders"));
            }
            let (s, ty) = src_expr(src, f32_defs)?;
            if ty != ScalarTy::F32 {
                return Err(err("dsx only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dsx destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("dpdx({s})"), modifiers)?;
            let dst_name = reg_var_name(&dst.reg)?;
            Some(emit_assign(
                dst,
                format!("select({dst_name}, {e}, {cond})"),
            )?)
        }
        IrOp::Ddy {
            dst,
            src,
            modifiers,
        } => {
            if stage != ShaderStage::Pixel {
                return Err(err("dsy is only supported in pixel shaders"));
            }
            let (s, ty) = src_expr(src, f32_defs)?;
            if ty != ScalarTy::F32 {
                return Err(err("dsy only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dsy destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("dpdy({s})"), modifiers)?;
            let dst_name = reg_var_name(&dst.reg)?;
            Some(emit_assign(
                dst,
                format!("select({dst_name}, {e}, {cond})"),
            )?)
        }
        IrOp::TexSample {
            kind: crate::sm3::ir::TexSampleKind::ImplicitLod { project },
            dst,
            coord,
            sampler,
            modifiers,
            ..
        } if stage == ShaderStage::Pixel => {
            let (coord_e, coord_ty) = src_expr(coord, f32_defs)?;
            if coord_ty != ScalarTy::F32 {
                return Err(err("texsample coordinate must be float"));
            }

            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("texsample destination must be float"));
            }

            let tex_ty = sampler_types
                .get(sampler)
                .copied()
                .unwrap_or(TextureType::Texture2D);

            let tex = format!("tex{sampler}");
            let samp = format!("samp{sampler}");
            let coord = tex_coord_expr(&coord_e, tex_ty, *project)?;
            let sample = format!("textureSample({tex}, {samp}, {coord})");
            let sample = apply_float_result_modifiers(sample, modifiers)?;

            let dst_name = reg_var_name(&dst.reg)?;
            Some(emit_assign(
                dst,
                format!("select({dst_name}, {sample}, {cond})"),
            )?)
        }
        IrOp::TexSample {
            kind: crate::sm3::ir::TexSampleKind::Bias,
            dst,
            coord,
            sampler,
            modifiers,
            ..
        } if stage == ShaderStage::Pixel => {
            let (coord_e, coord_ty) = src_expr(coord, f32_defs)?;
            if coord_ty != ScalarTy::F32 {
                return Err(err("texsample coordinate must be float"));
            }

            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("texsample destination must be float"));
            }

            let tex_ty = sampler_types
                .get(sampler)
                .copied()
                .unwrap_or(TextureType::Texture2D);

            let tex = format!("tex{sampler}");
            let samp = format!("samp{sampler}");
            let coord = tex_coord_expr(&coord_e, tex_ty, false)?;
            let bias = format!("({coord_e}).w");
            let sample = format!("textureSampleBias({tex}, {samp}, {coord}, {bias})");
            let sample = apply_float_result_modifiers(sample, modifiers)?;

            let dst_name = reg_var_name(&dst.reg)?;
            Some(emit_assign(
                dst,
                format!("select({dst_name}, {sample}, {cond})"),
            )?)
        }
        _ => None,
    })
}

fn combine_guard_conditions(a: &str, b: &str) -> String {
    if a == "true" {
        b.to_owned()
    } else if b == "true" {
        a.to_owned()
    } else {
        format!("({a} && {b})")
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_block_predicated(
    wgsl: &mut String,
    block: &Block,
    guard: &str,
    indent: usize,
    depth: usize,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
    state: &mut EmitState,
) -> Result<(), WgslError> {
    if depth > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        return Err(err(format!(
            "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
        )));
    }
    for stmt in &block.stmts {
        emit_stmt_predicated(
            wgsl,
            stmt,
            guard,
            indent,
            depth,
            stage,
            f32_defs,
            sampler_types,
            subroutine_infos,
            state,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_stmt_predicated(
    wgsl: &mut String,
    stmt: &Stmt,
    guard: &str,
    indent: usize,
    depth: usize,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
    state: &mut EmitState,
) -> Result<(), WgslError> {
    let pad = "  ".repeat(indent);
    match stmt {
        Stmt::Loop { init, body } => {
            if guard == "true" {
                emit_loop_stmt(
                    wgsl,
                    init,
                    body,
                    indent,
                    depth,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                    "true",
                )?;
            } else if block_contains_uniformity_sensitive_ops(body, stage, subroutine_infos) {
                // If this loop contains uniformity-sensitive ops (derivatives / implicit sampling),
                // avoid guarding the whole loop with `if (guard) { ... }` which would place those
                // ops behind non-uniform control flow. Instead, execute the loop unconditionally
                // and predicate its body.
                emit_loop_stmt(
                    wgsl,
                    init,
                    body,
                    indent,
                    depth,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                    guard,
                )?;
            } else {
                let _ = writeln!(wgsl, "{pad}if ({guard}) {{");
                emit_loop_stmt(
                    wgsl,
                    init,
                    body,
                    indent + 1,
                    depth + 1,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                    "true",
                )?;
                let _ = writeln!(wgsl, "{pad}}}");
            }
        }
        Stmt::Rep { count_reg, body } => {
            if guard == "true" {
                emit_rep_stmt(
                    wgsl,
                    count_reg,
                    body,
                    indent,
                    depth,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                    "true",
                )?;
            } else if block_contains_uniformity_sensitive_ops(body, stage, subroutine_infos) {
                emit_rep_stmt(
                    wgsl,
                    count_reg,
                    body,
                    indent,
                    depth,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                    guard,
                )?;
            } else {
                let _ = writeln!(wgsl, "{pad}if ({guard}) {{");
                emit_rep_stmt(
                    wgsl,
                    count_reg,
                    body,
                    indent + 1,
                    depth + 1,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                    "true",
                )?;
                let _ = writeln!(wgsl, "{pad}}}");
            }
        }
        Stmt::Op(op) => {
            let mut cond = guard.to_owned();
            if let Some(pred) = &op_modifiers(op).predicate {
                let pred_cond = predicate_expr(pred)?;
                cond = combine_guard_conditions(&cond, &pred_cond);
            }

            if let Some(line) =
                emit_branchless_predicated_op_line(op, &cond, stage, f32_defs, sampler_types)?
            {
                let _ = writeln!(wgsl, "{pad}{line}");
                return Ok(());
            }

            if let Some(line) = emit_branchless_predicated_mov_line(op, &cond, f32_defs)? {
                let _ = writeln!(wgsl, "{pad}{line}");
                return Ok(());
            }

            let line = emit_op_line(op, stage, f32_defs, sampler_types)?;
            if cond == "true" {
                let _ = writeln!(wgsl, "{pad}{line}");
            } else {
                let _ = writeln!(wgsl, "{pad}if ({cond}) {{");
                let inner_pad = "  ".repeat(indent + 1);
                let _ = writeln!(wgsl, "{inner_pad}{line}");
                let _ = writeln!(wgsl, "{pad}}}");
            }
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
        } => {
            let cond_e = cond_expr(cond, f32_defs)?;
            let then_guard = combine_guard_conditions(guard, &cond_e);
            emit_block_predicated(
                wgsl,
                then_block,
                &then_guard,
                indent,
                depth + 1,
                stage,
                f32_defs,
                sampler_types,
                subroutine_infos,
                state,
            )?;
            if let Some(else_block) = else_block.as_ref() {
                let else_guard = combine_guard_conditions(guard, &format!("!({cond_e})"));
                emit_block_predicated(
                    wgsl,
                    else_block,
                    &else_guard,
                    indent,
                    depth + 1,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                )?;
            }
        }
        Stmt::Call { label } => {
            let info = subroutine_infos
                .get(label)
                .ok_or_else(|| err(format!("call target label l{label} is not defined")))?;
            if guard == "true" {
                let _ = writeln!(wgsl, "{pad}aero_sub_l{label}();");
            } else if info.uses_derivatives {
                emit_speculative_call_with_rollback(
                    wgsl,
                    indent,
                    guard,
                    *label,
                    subroutine_infos,
                    state,
                )?;
            } else {
                let _ = writeln!(wgsl, "{pad}if ({guard}) {{");
                let inner_pad = "  ".repeat(indent + 1);
                let _ = writeln!(wgsl, "{inner_pad}aero_sub_l{label}();");
                let _ = writeln!(wgsl, "{pad}}}");
            }
        }
        other => {
            if guard == "true" {
                emit_stmt(
                    wgsl,
                    other,
                    indent,
                    depth,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                )?;
                return Ok(());
            }

            // Conservatively preserve semantics for non-op statements by guarding them with an `if`.
            // This is only used as a fallback for complex `if` trees containing uniformity-sensitive
            // ops, where emitting those ops behind a non-uniform branch would be rejected by naga.
            let _ = writeln!(wgsl, "{pad}if ({guard}) {{");
            emit_stmt(
                wgsl,
                other,
                indent + 1,
                depth + 1,
                stage,
                f32_defs,
                sampler_types,
                subroutine_infos,
                state,
            )?;
            let _ = writeln!(wgsl, "{pad}}}");
        }
    }
    Ok(())
}

fn emit_branchless_predicated_mov_line(
    op: &IrOp,
    cond: &str,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
) -> Result<Option<String>, WgslError> {
    Ok(match op {
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
            let dst_name = reg_var_name(&dst.reg)?;
            Some(emit_assign(
                dst,
                format!("select({dst_name}, {src_e}, {cond})"),
            )?)
        }
        _ => None,
    })
}

fn emit_op_line(
    op: &IrOp,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
) -> Result<String, WgslError> {
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
        IrOp::Abs {
            dst,
            src,
            modifiers,
        } => {
            let (s, ty) = src_expr(src, f32_defs)?;
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if ty != dst_ty {
                return Err(err("abs between mismatched types"));
            }
            match ty {
                ScalarTy::F32 => {
                    let e = apply_float_result_modifiers(format!("abs({s})"), modifiers)?;
                    emit_assign(dst, e)
                }
                ScalarTy::I32 => {
                    if modifiers.saturate || modifiers.shift != ResultShift::None {
                        return Err(err("result modifiers not supported for integer abs"));
                    }
                    emit_assign(dst, format!("abs({s})"))
                }
                ScalarTy::Bool => Err(err("abs on bool source")),
            }
        }
        IrOp::Sgn {
            dst,
            src,
            modifiers,
        } => {
            let (s, ty) = src_expr(src, f32_defs)?;
            if ty != ScalarTy::F32 {
                return Err(err("sgn only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("sgn destination must be float"));
            }
            let e = apply_float_result_modifiers(format!("sign({s})"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::Exp {
            dst,
            src,
            modifiers,
        } => emit_float_func1(dst, src, modifiers, f32_defs, "exp2"),
        IrOp::Log {
            dst,
            src,
            modifiers,
        } => emit_float_func1(dst, src, modifiers, f32_defs, "log2"),
        IrOp::Ddx {
            dst,
            src,
            modifiers,
        } => {
            if stage != ShaderStage::Pixel {
                return Err(err("dsx is only supported in pixel shaders"));
            }
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
        IrOp::Ddy {
            dst,
            src,
            modifiers,
        } => {
            if stage != ShaderStage::Pixel {
                return Err(err("dsy is only supported in pixel shaders"));
            }
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
        IrOp::Nrm {
            dst,
            src,
            modifiers,
        } => {
            let (s, ty) = src_expr(src, f32_defs)?;
            if ty != ScalarTy::F32 {
                return Err(err("nrm only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("nrm destination must be float"));
            }
            // D3D9 `nrm`: normalize src.xyz; the W component is not well-specified,
            // but most shaders only consume `.xyz`. Set W to 1.0 for deterministic output.
            let e = apply_float_result_modifiers(
                format!("vec4<f32>(normalize(({s}).xyz), 1.0)"),
                modifiers,
            )?;
            emit_assign(dst, e)
        }
        IrOp::Lit {
            dst,
            src,
            modifiers,
        } => {
            let (s, ty) = src_expr(src, f32_defs)?;
            if ty != ScalarTy::F32 {
                return Err(err("lit only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("lit destination must be float"));
            }
            // D3D9 `lit`:
            //   dst.x = 1
            //   dst.y = max(src.x, 0)
            //   dst.z = (src.x > 0) ? pow(max(src.y, 0), src.w) : 0
            //   dst.w = 1
            let sx = format!("({s}).x");
            let sy = format!("({s}).y");
            let sw = format!("({s}).w");
            let y = format!("max({sx}, 0.0)");
            let z = format!("select(0.0, pow(max({sy}, 0.0), {sw}), ({sx} > 0.0))");
            let e =
                apply_float_result_modifiers(format!("vec4<f32>(1.0, {y}, {z}, 1.0)"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::SinCos {
            dst,
            src,
            src1,
            src2,
            modifiers,
        } => {
            let (s0, ty0) = src_expr(src, f32_defs)?;
            if ty0 != ScalarTy::F32 {
                return Err(err("sincos only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("sincos destination must be float"));
            }
            let angle = match (src1, src2) {
                (None, None) => format!("({s0}).x"),
                (Some(src1), Some(src2)) => {
                    let (s1, ty1) = src_expr(src1, f32_defs)?;
                    let (s2, ty2) = src_expr(src2, f32_defs)?;
                    if ty1 != ScalarTy::F32 || ty2 != ScalarTy::F32 {
                        return Err(err(
                            "sincos scale/offset operands must be float in WGSL lowering",
                        ));
                    }
                    // D3D9 `sincos` optionally scales/biases the angle:
                    //   angle = src0.x * src1.x + src2.x
                    format!("(({s0}).x * ({s1}).x + ({s2}).x)")
                }
                _ => {
                    return Err(err(
                        "sincos must have either 1 or 3 source operands in WGSL lowering",
                    ))
                }
            };
            // WGSL trig functions operate on radians; D3D9 SM2/3 `sincos` is specified in radians.
            let e = apply_float_result_modifiers(
                format!("vec4<f32>(sin({angle}), cos({angle}), 0.0, 0.0)"),
                modifiers,
            )?;
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
        IrOp::Dp2Add {
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
                return Err(err("dp2add only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dp2add destination must be float"));
            }
            let dot = format!("dot(({a}).xy, ({b}).xy)");
            let add = format!("({c}).x");
            let e = apply_float_result_modifiers(format!("vec4<f32>({dot} + {add})"), modifiers)?;
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
        IrOp::Dst {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
            if aty != ScalarTy::F32 || bty != ScalarTy::F32 {
                return Err(err("dst only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("dst destination must be float"));
            }
            // D3D9 `dst`: x is 1.0; y is src0.y * src1.y; z is src0.z; w is src1.w.
            let e = apply_float_result_modifiers(
                format!("vec4<f32>(1.0, ({a}).y * ({b}).y, ({a}).z, ({b}).w)"),
                modifiers,
            )?;
            emit_assign(dst, e)
        }
        IrOp::Crs {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            let (a, aty) = src_expr(src0, f32_defs)?;
            let (b, bty) = src_expr(src1, f32_defs)?;
            if aty != ScalarTy::F32 || bty != ScalarTy::F32 {
                return Err(err("crs only supports float sources in WGSL lowering"));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("crs destination must be float"));
            }
            // D3D9 `crs`: cross product of the xyz components. The W component is not well-specified,
            // but most shaders only consume `.xyz`. Set W to 1.0 for deterministic output.
            let cross = format!("cross(({a}).xyz, ({b}).xyz)");
            let e = apply_float_result_modifiers(format!("vec4<f32>({cross}, 1.0)"), modifiers)?;
            emit_assign(dst, e)
        }
        IrOp::MatrixMul {
            dst,
            src0,
            src1,
            m,
            n,
            modifiers,
        } => {
            let (v, vty) = src_expr(src0, f32_defs)?;
            if vty != ScalarTy::F32 {
                return Err(err(
                    "matrix multiply only supports float vectors in WGSL lowering",
                ));
            }
            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("matrix multiply destination must be float"));
            }

            let mut dots = Vec::new();
            for col in 0..*n {
                let mut column = src1.clone();
                column.reg.index = column
                    .reg
                    .index
                    .checked_add(u32::from(col))
                    .ok_or_else(|| err("matrix multiply constant index overflow"))?;
                let (mexpr, mty) = src_expr(&column, f32_defs)?;
                if mty != ScalarTy::F32 {
                    return Err(err(
                        "matrix multiply only supports float matrices in WGSL lowering",
                    ));
                }
                let dot = match *m {
                    4 => format!("dot(({v}), ({mexpr}))"),
                    3 => format!("dot(({v}).xyz, ({mexpr}).xyz)"),
                    2 => format!("dot(({v}).xy, ({mexpr}).xy)"),
                    other => {
                        return Err(err(format!(
                            "unsupported matrix multiply operand size m={other}"
                        )))
                    }
                };
                dots.push(dot);
            }
            while dots.len() < 4 {
                dots.push("0.0".to_owned());
            }
            let raw = format!(
                "vec4<f32>({}, {}, {}, {})",
                dots[0], dots[1], dots[2], dots[3]
            );
            let modded = apply_float_result_modifiers(raw, modifiers)?;

            let dst_name = reg_var_name(&dst.reg)?;
            let final_vec = match *n {
                4 => modded,
                3 => format!("vec4<f32>(({modded}).xyz, ({dst_name}).w)"),
                2 => format!("vec4<f32>(({modded}).xy, ({dst_name}).z, ({dst_name}).w)"),
                1 => format!(
                    "vec4<f32>(({modded}).x, ({dst_name}).y, ({dst_name}).z, ({dst_name}).w)"
                ),
                other => {
                    return Err(err(format!(
                        "unsupported matrix multiply output size n={other}"
                    )))
                }
            };
            emit_assign(dst, final_vec)
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
                    if aty == ScalarTy::Bool && !matches!(op, CompareOp::Eq | CompareOp::Ne) {
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
        IrOp::TexSample {
            kind,
            dst,
            coord,
            ddx,
            ddy,
            sampler,
            modifiers,
        } => {
            let (coord_e, coord_ty) = src_expr(coord, f32_defs)?;
            if coord_ty != ScalarTy::F32 {
                return Err(err("texsample coordinate must be float"));
            }

            let dst_ty = reg_scalar_ty(dst.reg.file).ok_or_else(|| err("unsupported dst file"))?;
            if dst_ty != ScalarTy::F32 {
                return Err(err("texsample destination must be float"));
            }

            let tex_ty = sampler_types
                .get(sampler)
                .copied()
                .unwrap_or(TextureType::Texture2D);

            let tex = format!("tex{sampler}");
            let samp = format!("samp{sampler}");

            let sample = match kind {
                crate::sm3::ir::TexSampleKind::ImplicitLod { project } => {
                    let uv = tex_coord_expr(&coord_e, tex_ty, *project)?;
                    match stage {
                        // Vertex stage has no implicit derivatives, so use an explicit LOD.
                        ShaderStage::Vertex => {
                            format!("textureSampleLevel({tex}, {samp}, {uv}, 0.0)")
                        }
                        ShaderStage::Pixel => format!("textureSample({tex}, {samp}, {uv})"),
                    }
                }
                crate::sm3::ir::TexSampleKind::Bias => {
                    if stage != ShaderStage::Pixel {
                        return Err(err(
                            "texldb/Bias texture sampling is only supported in pixel shaders",
                        ));
                    }
                    let uv = tex_coord_expr(&coord_e, tex_ty, false)?;
                    let bias = format!("({coord_e}).w");
                    format!("textureSampleBias({tex}, {samp}, {uv}, {bias})")
                }
                crate::sm3::ir::TexSampleKind::ExplicitLod => {
                    let uv = tex_coord_expr(&coord_e, tex_ty, false)?;
                    let lod = format!("({coord_e}).w");
                    format!("textureSampleLevel({tex}, {samp}, {uv}, {lod})")
                }
                crate::sm3::ir::TexSampleKind::Grad => {
                    if stage != ShaderStage::Pixel {
                        return Err(err(
                            "texldd/Grad texture sampling is only supported in pixel shaders",
                        ));
                    }
                    let ddx = ddx
                        .as_ref()
                        .ok_or_else(|| err("texldd missing ddx operand"))?;
                    let ddy = ddy
                        .as_ref()
                        .ok_or_else(|| err("texldd missing ddy operand"))?;
                    let (ddx_e, ddx_ty) = src_expr(ddx, f32_defs)?;
                    let (ddy_e, ddy_ty) = src_expr(ddy, f32_defs)?;
                    if ddx_ty != ScalarTy::F32 || ddy_ty != ScalarTy::F32 {
                        return Err(err("texldd gradients must be float"));
                    }
                    let uv = tex_coord_expr(&coord_e, tex_ty, false)?;
                    let ddx = tex_grad_expr(&ddx_e, tex_ty)?;
                    let ddy = tex_grad_expr(&ddy_e, tex_ty)?;
                    format!(
                        "textureSampleGrad({tex}, {samp}, {uv}, {ddx}, {ddy})"
                    )
                }
            };

            let sample = apply_float_result_modifiers(sample, modifiers)?;
            emit_assign(dst, sample)
        }
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

#[derive(Debug, Clone)]
struct LoopRestore {
    loop_reg: String,
    saved_var: String,
}

#[derive(Debug, Clone)]
struct EmitState {
    in_subroutine: bool,
    loop_stack: Vec<LoopRestore>,
    next_loop_id: u32,
    next_call_id: u32,
}

impl EmitState {
    fn new(in_subroutine: bool) -> Self {
        Self {
            in_subroutine,
            loop_stack: Vec::new(),
            next_loop_id: 0,
            next_call_id: 0,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_block(
    wgsl: &mut String,
    block: &Block,
    indent: usize,
    depth: usize,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
    state: &mut EmitState,
) -> Result<(), WgslError> {
    if depth > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        return Err(err(format!(
            "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
        )));
    }
    for stmt in &block.stmts {
        emit_stmt(
            wgsl,
            stmt,
            indent,
            depth,
            stage,
            f32_defs,
            sampler_types,
            subroutine_infos,
            state,
        )?;
    }
    Ok(())
}

fn emit_speculative_call_with_rollback(
    wgsl: &mut String,
    indent: usize,
    cond: &str,
    label: u32,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
    state: &mut EmitState,
) -> Result<(), WgslError> {
    let info = subroutine_infos
        .get(&label)
        .ok_or_else(|| err(format!("call target label l{label} is not defined")))?;

    let pad = "  ".repeat(indent);
    // If the subroutine does not write any registers and cannot discard, we can just execute it
    // unconditionally.
    //
    // Note: when a subroutine may discard, we still need to guard discard so speculative execution
    // does not terminate lanes that should not have taken the call.
    if info.writes.is_empty() && !info.may_discard {
        let _ = writeln!(wgsl, "{pad}aero_sub_l{label}();");
        return Ok(());
    }

    let call_id = state.next_call_id;
    state.next_call_id = state.next_call_id.wrapping_add(1);
    let taken_var = format!("_aero_call_taken_{call_id}");
    let _ = writeln!(wgsl, "{pad}let {taken_var}: bool = {cond};");

    // Snapshot registers written by the callee (transitively).
    let mut saved_vars = Vec::new();
    for &(file, index) in &info.writes {
        let reg = RegRef {
            file,
            index,
            relative: None,
        };
        let name = reg_var_name(&reg)?;
        let ty = reg_scalar_ty(file).ok_or_else(|| err("unsupported subroutine dst file"))?;
        let saved = format!("_aero_saved_call{call_id}_{name}");
        let _ = writeln!(wgsl, "{pad}let {saved}: {} = {name};", ty.wgsl_vec4());
        saved_vars.push((name, saved));
    }

    // If the call is conditional and the callee can discard, suppress discard when the call should
    // not have been taken.
    //
    // This uses a single module-scope guard variable shared by all speculative calls. We save and
    // restore it so nested calls compose correctly.
    let mut saved_guard_var = None::<String>;
    if info.may_discard {
        let saved = format!("_aero_saved_call_guard_{call_id}");
        let _ = writeln!(wgsl, "{pad}let {saved}: bool = _aero_call_guard;");
        // Combine with any outer guard (e.g. nested speculative calls).
        let _ = writeln!(wgsl, "{pad}_aero_call_guard = ({saved} && {taken_var});");
        saved_guard_var = Some(saved);
    }

    // Execute the call on all lanes.
    let _ = writeln!(wgsl, "{pad}aero_sub_l{label}();");

    // Restore discard guard.
    if let Some(saved_guard) = saved_guard_var {
        let _ = writeln!(wgsl, "{pad}_aero_call_guard = {saved_guard};");
    }

    // Roll back register writes when the call should not have been taken.
    if !saved_vars.is_empty() {
        let _ = writeln!(wgsl, "{pad}if (!({taken_var})) {{");
        let inner_pad = "  ".repeat(indent + 1);
        for (name, saved) in saved_vars {
            let _ = writeln!(wgsl, "{inner_pad}{name} = {saved};");
        }
        let _ = writeln!(wgsl, "{pad}}}");
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_loop_stmt(
    wgsl: &mut String,
    init: &crate::sm3::ir::LoopInit,
    body: &Block,
    indent: usize,
    depth: usize,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
    state: &mut EmitState,
    body_guard: &str,
) -> Result<(), WgslError> {
    // D3D9 SM2/3 `loop aL, i#` has a finite trip count derived from the integer constant register
    // (i#.x=start, i#.y=end, i#.z=step). We must not emit an unbounded WGSL `loop {}` because it
    // can hang the GPU on malformed shaders.
    //
    // Emit a conservative bounded loop:
    // - Break if step==0.
    // - Break if the loop counter moves past the end bound.
    // - Break if a safety cap is exceeded.
    const MAX_ITERS: u32 = 1024;

    if init.loop_reg.file != RegFile::Loop {
        return Err(err("loop init uses a non-loop register"));
    }
    if init.ctrl_reg.file != RegFile::ConstInt {
        return Err(err("loop init uses a non-integer-constant register"));
    }
    if init.loop_reg.relative.is_some() || init.ctrl_reg.relative.is_some() {
        return Err(err("relative addressing is not supported in loop operands"));
    }

    let pad = "  ".repeat(indent);
    let loop_reg = reg_var_name(&init.loop_reg)?;
    let ctrl = reg_var_name(&init.ctrl_reg)?;

    let pad1 = "  ".repeat(indent + 1);
    let pad2 = "  ".repeat(indent + 2);
    let loop_id = state.next_loop_id;
    state.next_loop_id = state.next_loop_id.wrapping_add(1);
    let saved_loop_reg = format!("_aero_saved_loop_reg_{loop_id}");

    let _ = writeln!(wgsl, "{pad}{{");
    let _ = writeln!(wgsl, "{pad1}var _aero_loop_iter: u32 = 0u;");
    let _ = writeln!(wgsl, "{pad1}let {saved_loop_reg}: vec4<i32> = {loop_reg};");
    let _ = writeln!(wgsl, "{pad1}let _aero_loop_end: i32 = ({ctrl}).y;");
    let _ = writeln!(wgsl, "{pad1}let _aero_loop_step: i32 = ({ctrl}).z;");
    let _ = writeln!(wgsl, "{pad1}{loop_reg}.x = ({ctrl}).x;");
    let _ = writeln!(wgsl, "{pad1}loop {{");

    let _ = writeln!(
        wgsl,
        "{pad2}if (_aero_loop_iter >= {MAX_ITERS}u) {{ break; }}"
    );
    let _ = writeln!(wgsl, "{pad2}if (_aero_loop_step == 0) {{ break; }}");
    let _ = writeln!(
        wgsl,
        "{pad2}if ((_aero_loop_step > 0 && {loop_reg}.x > _aero_loop_end) || (_aero_loop_step < 0 && {loop_reg}.x < _aero_loop_end)) {{ break; }}"
    );

    state.loop_stack.push(LoopRestore {
        loop_reg: loop_reg.clone(),
        saved_var: saved_loop_reg.clone(),
    });
    if body_guard == "true" {
        emit_block(
            wgsl,
            body,
            indent + 2,
            depth + 1,
            stage,
            f32_defs,
            sampler_types,
            subroutine_infos,
            state,
        )?;
    } else {
        emit_block_predicated(
            wgsl,
            body,
            body_guard,
            indent + 2,
            depth + 1,
            stage,
            f32_defs,
            sampler_types,
            subroutine_infos,
            state,
        )?;
    }
    state.loop_stack.pop();

    let _ = writeln!(wgsl, "{pad2}{loop_reg}.x = {loop_reg}.x + _aero_loop_step;");
    let _ = writeln!(wgsl, "{pad2}_aero_loop_iter = _aero_loop_iter + 1u;");
    let _ = writeln!(wgsl, "{pad1}}}");
    let _ = writeln!(wgsl, "{pad1}{loop_reg} = {saved_loop_reg};");
    let _ = writeln!(wgsl, "{pad}}}");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_rep_stmt(
    wgsl: &mut String,
    count_reg: &RegRef,
    body: &Block,
    indent: usize,
    depth: usize,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
    state: &mut EmitState,
    body_guard: &str,
) -> Result<(), WgslError> {
    // D3D9 SM2/3 `rep i#` repeats the block `i#.x` times using `aL.x` as the loop counter.
    //
    // Emit a bounded WGSL loop to avoid hanging the GPU on malformed shaders.
    const MAX_ITERS: u32 = 1024;

    if count_reg.file != RegFile::ConstInt {
        return Err(err("rep init uses a non-integer-constant register"));
    }
    if count_reg.relative.is_some() {
        return Err(err(
            "relative addressing is not supported in rep count operands",
        ));
    }

    let pad = "  ".repeat(indent);
    let loop_reg = reg_var_name(&RegRef {
        file: RegFile::Loop,
        index: 0,
        relative: None,
    })?;
    let count = reg_var_name(count_reg)?;

    let pad1 = "  ".repeat(indent + 1);
    let pad2 = "  ".repeat(indent + 2);
    let loop_id = state.next_loop_id;
    state.next_loop_id = state.next_loop_id.wrapping_add(1);
    let saved_loop_reg = format!("_aero_saved_loop_reg_{loop_id}");

    let _ = writeln!(wgsl, "{pad}{{");
    let _ = writeln!(wgsl, "{pad1}var _aero_loop_iter: u32 = 0u;");
    let _ = writeln!(wgsl, "{pad1}let {saved_loop_reg}: vec4<i32> = {loop_reg};");
    let _ = writeln!(wgsl, "{pad1}let _aero_rep_count: i32 = ({count}).x;");
    let _ = writeln!(wgsl, "{pad1}{loop_reg}.x = 0;");
    let _ = writeln!(wgsl, "{pad1}loop {{");

    let _ = writeln!(
        wgsl,
        "{pad2}if (_aero_loop_iter >= {MAX_ITERS}u) {{ break; }}"
    );
    let _ = writeln!(
        wgsl,
        "{pad2}if ({loop_reg}.x >= _aero_rep_count) {{ break; }}"
    );

    state.loop_stack.push(LoopRestore {
        loop_reg: loop_reg.clone(),
        saved_var: saved_loop_reg.clone(),
    });
    if body_guard == "true" {
        emit_block(
            wgsl,
            body,
            indent + 2,
            depth + 1,
            stage,
            f32_defs,
            sampler_types,
            subroutine_infos,
            state,
        )?;
    } else {
        emit_block_predicated(
            wgsl,
            body,
            body_guard,
            indent + 2,
            depth + 1,
            stage,
            f32_defs,
            sampler_types,
            subroutine_infos,
            state,
        )?;
    }
    state.loop_stack.pop();

    let _ = writeln!(wgsl, "{pad2}{loop_reg}.x = {loop_reg}.x + 1;");
    let _ = writeln!(wgsl, "{pad2}_aero_loop_iter = _aero_loop_iter + 1u;");
    let _ = writeln!(wgsl, "{pad1}}}");
    let _ = writeln!(wgsl, "{pad1}{loop_reg} = {saved_loop_reg};");
    let _ = writeln!(wgsl, "{pad}}}");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_stmt(
    wgsl: &mut String,
    stmt: &Stmt,
    indent: usize,
    depth: usize,
    stage: ShaderStage,
    f32_defs: &BTreeMap<u32, [f32; 4]>,
    sampler_types: &HashMap<u32, TextureType>,
    subroutine_infos: &HashMap<u32, SubroutineInfo>,
    state: &mut EmitState,
) -> Result<(), WgslError> {
    let pad = "  ".repeat(indent);
    match stmt {
        Stmt::Op(op) => {
            if let Some(pred) = &op_modifiers(op).predicate {
                // WGSL derivative ops (`dpdx`/`dpdy`) and implicit-derivative texture sampling
                // (`textureSample`/`textureSampleBias`) must be in *uniform control flow*. Wrapping
                // them in an `if` for SM3 predicate modifiers can cause naga uniformity validation
                // errors when the predicate is non-uniform.
                //
                // For these ops, lower predication to unconditional evaluation + conditional
                // assignment via `select`, which does not introduce control flow.
                let pred_cond = predicate_expr(pred)?;
                if let Some(line) = emit_branchless_predicated_op_line(
                    op,
                    &pred_cond,
                    stage,
                    f32_defs,
                    sampler_types,
                )? {
                    let _ = writeln!(wgsl, "{pad}{line}");
                } else {
                    let _ = writeln!(wgsl, "{pad}if ({pred_cond}) {{");
                    let line = emit_op_line(op, stage, f32_defs, sampler_types)?;
                    let inner_pad = "  ".repeat(indent + 1);
                    let _ = writeln!(wgsl, "{inner_pad}{line}");
                    let _ = writeln!(wgsl, "{pad}}}");
                }
            } else {
                let line = emit_op_line(op, stage, f32_defs, sampler_types)?;
                let _ = writeln!(wgsl, "{pad}{line}");
            }
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
        } => {
            // Apply the same uniform-control-flow workaround as predicate modifiers for the common
            // patterns (and slight generalizations of them):
            //
            //   if (cond) { <single op>; }
            //   if (cond) { <single op>; } else { <single op>; }
            //   if (cond) { <sensitive op>; <...>; }
            //   if (cond) { <...>; } else { <sensitive op>; <...>; }
            //
            // This avoids generating WGSL that places `dpdx`/`dpdy`/`textureSample`/`textureSampleBias`
            // behind a potentially non-uniform branch.
            let cond_e = cond_expr(cond, f32_defs)?;
            let not_cond_e = format!("!({cond_e})");

            let contains_sensitive =
                block_contains_uniformity_sensitive_ops(then_block, stage, subroutine_infos)
                    || else_block.as_ref().is_some_and(|b| {
                        block_contains_uniformity_sensitive_ops(b, stage, subroutine_infos)
                    });

            let cond_with_pred = |op: &IrOp, base_cond: &str| -> Result<String, WgslError> {
                if let Some(pred) = &op_modifiers(op).predicate {
                    let pred_cond = predicate_expr(pred)?;
                    Ok(format!("({base_cond} && {pred_cond})"))
                } else {
                    Ok(base_cond.to_owned())
                }
            };

            // Uniformity-sensitive ops can only be hoisted if they appear before any other
            // statements in the branch (so they don't depend on branch-local definitions).
            //
            // Note: it is safe for the op to execute on *all* lanes (outside the `if`), because its
            // assignment is guarded by a `select` and it has no side effects other than writing its
            // destination register.
            let mut then_skip = 0usize;
            let mut else_skip = 0usize;
            let mut then_lines = Vec::<String>::new();
            let mut else_lines = Vec::<String>::new();
            let mut then_call: Option<(String, u32)> = None;
            let mut else_call: Option<(String, u32)> = None;

            let call_chain =
                |stmt: &Stmt, base_cond: &str| -> Result<Option<(String, u32)>, WgslError> {
                    let mut cond = base_cond.to_owned();
                    let mut current = stmt;
                    loop {
                        match current {
                            Stmt::Call { label } => return Ok(Some((cond, *label))),
                            Stmt::If {
                                cond: inner_cond,
                                then_block,
                                else_block: None,
                            } => {
                                if then_block.stmts.len() != 1 {
                                    return Ok(None);
                                }
                                let inner = cond_expr(inner_cond, f32_defs)?;
                                cond = format!("({cond} && {inner})");
                                current = &then_block.stmts[0];
                            }
                            _ => return Ok(None),
                        }
                    }
                };

            for stmt in &then_block.stmts {
                let Stmt::Op(op) = stmt else {
                    break;
                };
                let cond_for_op = cond_with_pred(op, &cond_e)?;
                let Some(line) = emit_branchless_predicated_op_line(
                    op,
                    &cond_for_op,
                    stage,
                    f32_defs,
                    sampler_types,
                )?
                else {
                    break;
                };
                then_lines.push(line);
                then_skip += 1;
            }

            if let Some(else_b) = else_block.as_ref() {
                for stmt in &else_b.stmts {
                    let Stmt::Op(op) = stmt else {
                        break;
                    };
                    let cond_for_op = cond_with_pred(op, &not_cond_e)?;
                    let Some(line) = emit_branchless_predicated_op_line(
                        op,
                        &cond_for_op,
                        stage,
                        f32_defs,
                        sampler_types,
                    )?
                    else {
                        break;
                    };
                    else_lines.push(line);
                    else_skip += 1;
                }
            }

            // If the first op isn't sensitive but the second op is, we can hoist both by also
            // lowering the prefix `mov` to a predicated `select` assignment.
            if then_lines.is_empty() && then_block.stmts.len() >= 2 {
                if let (Some(Stmt::Op(mov)), Some(Stmt::Op(op))) =
                    (then_block.stmts.first(), then_block.stmts.get(1))
                {
                    let mov_cond = cond_with_pred(mov, &cond_e)?;
                    if let Some(mov_line) =
                        emit_branchless_predicated_mov_line(mov, &mov_cond, f32_defs)?
                    {
                        let op_cond = cond_with_pred(op, &cond_e)?;
                        if let Some(op_line) = emit_branchless_predicated_op_line(
                            op,
                            &op_cond,
                            stage,
                            f32_defs,
                            sampler_types,
                        )? {
                            then_lines.push(mov_line);
                            then_lines.push(op_line);
                            then_skip = 2;
                        }
                    }
                }
            }

            if then_lines.is_empty() {
                if let Some(first) = then_block.stmts.first() {
                    if let Some((call_cond, label)) = call_chain(first, &cond_e)? {
                        let sub_info = subroutine_infos.get(&label).ok_or_else(|| {
                            err(format!("call target label l{label} is not defined"))
                        })?;
                        if sub_info.uses_derivatives {
                            then_call = Some((call_cond, label));
                            then_skip = 1;
                        }
                    }
                }
            }

            if else_lines.is_empty() {
                if let Some(else_b) = else_block.as_ref() {
                    if else_b.stmts.len() >= 2 {
                        if let (Some(Stmt::Op(mov)), Some(Stmt::Op(op))) =
                            (else_b.stmts.first(), else_b.stmts.get(1))
                        {
                            let mov_cond = cond_with_pred(mov, &not_cond_e)?;
                            if let Some(mov_line) =
                                emit_branchless_predicated_mov_line(mov, &mov_cond, f32_defs)?
                            {
                                let op_cond = cond_with_pred(op, &not_cond_e)?;
                                if let Some(op_line) = emit_branchless_predicated_op_line(
                                    op,
                                    &op_cond,
                                    stage,
                                    f32_defs,
                                    sampler_types,
                                )? {
                                    else_lines.push(mov_line);
                                    else_lines.push(op_line);
                                    else_skip = 2;
                                }
                            }
                        }
                    }
                }
            }
            if else_lines.is_empty() {
                if let Some(else_b) = else_block.as_ref() {
                    if let Some(first) = else_b.stmts.first() {
                        if let Some((call_cond, label)) = call_chain(first, &not_cond_e)? {
                            let sub_info = subroutine_infos.get(&label).ok_or_else(|| {
                                err(format!("call target label l{label} is not defined"))
                            })?;
                            if sub_info.uses_derivatives {
                                else_call = Some((call_cond, label));
                                else_skip = 1;
                            }
                        }
                    }
                }
            }

            let then_rest = &then_block.stmts[then_skip..];
            let else_rest = else_block
                .as_ref()
                .map(|b| &b.stmts[else_skip..])
                .unwrap_or(&[]);

            let hoisted_any = !then_lines.is_empty()
                || !else_lines.is_empty()
                || then_call.is_some()
                || else_call.is_some();

            // If we can't hoist all uniformity-sensitive ops out of the `if` using the prefix
            // patterns above (e.g. sensitive ops occur later in a branch or are nested), fall back
            // to emitting the whole `if` in a predicated form.
            if contains_sensitive
                && (!hoisted_any
                    || block_contains_uniformity_sensitive_ops(
                        &Block {
                            stmts: then_rest.to_vec(),
                        },
                        stage,
                        subroutine_infos,
                    )
                    || block_contains_uniformity_sensitive_ops(
                        &Block {
                            stmts: else_rest.to_vec(),
                        },
                        stage,
                        subroutine_infos,
                    ))
            {
                emit_block_predicated(
                    wgsl,
                    then_block,
                    &cond_e,
                    indent,
                    depth + 1,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                )?;
                if let Some(else_block) = else_block.as_ref() {
                    emit_block_predicated(
                        wgsl,
                        else_block,
                        &not_cond_e,
                        indent,
                        depth + 1,
                        stage,
                        f32_defs,
                        sampler_types,
                        subroutine_infos,
                        state,
                    )?;
                }
                return Ok(());
            }

            if hoisted_any {
                for line in &then_lines {
                    let _ = writeln!(wgsl, "{pad}{line}");
                }
                for line in &else_lines {
                    let _ = writeln!(wgsl, "{pad}{line}");
                }

                if let Some((call_cond, label)) = &then_call {
                    emit_speculative_call_with_rollback(
                        wgsl,
                        indent,
                        call_cond,
                        *label,
                        subroutine_infos,
                        state,
                    )?;
                }
                if let Some((call_cond, label)) = &else_call {
                    emit_speculative_call_with_rollback(
                        wgsl,
                        indent,
                        call_cond,
                        *label,
                        subroutine_infos,
                        state,
                    )?;
                }

                match (!then_rest.is_empty(), !else_rest.is_empty()) {
                    (false, false) => return Ok(()),
                    (true, false) => {
                        let then_block = Block {
                            stmts: then_rest.to_vec(),
                        };
                        let _ = writeln!(wgsl, "{pad}if ({cond_e}) {{");
                        emit_block(
                            wgsl,
                            &then_block,
                            indent + 1,
                            depth + 1,
                            stage,
                            f32_defs,
                            sampler_types,
                            subroutine_infos,
                            state,
                        )?;
                        let _ = writeln!(wgsl, "{pad}}}");
                        return Ok(());
                    }
                    (false, true) => {
                        let else_block = Block {
                            stmts: else_rest.to_vec(),
                        };
                        let _ = writeln!(wgsl, "{pad}if ({not_cond_e}) {{");
                        emit_block(
                            wgsl,
                            &else_block,
                            indent + 1,
                            depth + 1,
                            stage,
                            f32_defs,
                            sampler_types,
                            subroutine_infos,
                            state,
                        )?;
                        let _ = writeln!(wgsl, "{pad}}}");
                        return Ok(());
                    }
                    (true, true) => {
                        let then_block = Block {
                            stmts: then_rest.to_vec(),
                        };
                        let else_block = Block {
                            stmts: else_rest.to_vec(),
                        };
                        let _ = writeln!(wgsl, "{pad}if ({cond_e}) {{");
                        emit_block(
                            wgsl,
                            &then_block,
                            indent + 1,
                            depth + 1,
                            stage,
                            f32_defs,
                            sampler_types,
                            subroutine_infos,
                            state,
                        )?;
                        let _ = writeln!(wgsl, "{pad}}} else {{");
                        emit_block(
                            wgsl,
                            &else_block,
                            indent + 1,
                            depth + 1,
                            stage,
                            f32_defs,
                            sampler_types,
                            subroutine_infos,
                            state,
                        )?;
                        let _ = writeln!(wgsl, "{pad}}}");
                        return Ok(());
                    }
                }
            }

            let _ = writeln!(wgsl, "{pad}if ({cond_e}) {{");
            emit_block(
                wgsl,
                then_block,
                indent + 1,
                depth + 1,
                stage,
                f32_defs,
                sampler_types,
                subroutine_infos,
                state,
            )?;
            if let Some(else_block) = else_block {
                let _ = writeln!(wgsl, "{pad}}} else {{");
                emit_block(
                    wgsl,
                    else_block,
                    indent + 1,
                    depth + 1,
                    stage,
                    f32_defs,
                    sampler_types,
                    subroutine_infos,
                    state,
                )?;
            }
            let _ = writeln!(wgsl, "{pad}}}");
        }
        Stmt::Loop { init, body } => {
            emit_loop_stmt(
                wgsl,
                init,
                body,
                indent,
                depth,
                stage,
                f32_defs,
                sampler_types,
                subroutine_infos,
                state,
                "true",
            )?;
        }
        Stmt::Rep { count_reg, body } => {
            emit_rep_stmt(
                wgsl,
                count_reg,
                body,
                indent,
                depth,
                stage,
                f32_defs,
                sampler_types,
                subroutine_infos,
                state,
                "true",
            )?;
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
            let (src_e, src_ty) = src_expr(src, f32_defs)?;
            if src_ty != ScalarTy::F32 {
                return Err(err("texkill requires a float source"));
            }

            let cond = if stage == ShaderStage::Pixel && state.in_subroutine {
                format!("(_aero_call_guard && any(({src_e}) < vec4<f32>(0.0)))")
            } else {
                format!("any(({src_e}) < vec4<f32>(0.0))")
            };
            let _ = writeln!(wgsl, "{pad}if ({cond}) {{");
            let inner_pad = "  ".repeat(indent + 1);
            let _ = writeln!(wgsl, "{inner_pad}discard;");
            let _ = writeln!(wgsl, "{pad}}}");
        }
        Stmt::Call { label } => {
            let _ = writeln!(wgsl, "{pad}aero_sub_l{label}();");
        }
        Stmt::Return => {
            if !state.in_subroutine {
                return Err(err("ret/return statement outside of a subroutine"));
            }
            // `return` can occur inside nested loops; restore the loop stack to match D3D9's loop
            // register save/restore semantics.
            for restore in state.loop_stack.iter().rev() {
                let _ = writeln!(wgsl, "{pad}{} = {};", restore.loop_reg, restore.saved_var);
            }
            let _ = writeln!(wgsl, "{pad}return;");
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
        | IrOp::Dp2Add { modifiers, .. }
        | IrOp::Dp3 { modifiers, .. }
        | IrOp::Dp4 { modifiers, .. }
        | IrOp::Dst { modifiers, .. }
        | IrOp::Crs { modifiers, .. }
        | IrOp::MatrixMul { modifiers, .. }
        | IrOp::Rcp { modifiers, .. }
        | IrOp::Rsq { modifiers, .. }
        | IrOp::Frc { modifiers, .. }
        | IrOp::Abs { modifiers, .. }
        | IrOp::Sgn { modifiers, .. }
        | IrOp::Exp { modifiers, .. }
        | IrOp::Log { modifiers, .. }
        | IrOp::Ddx { modifiers, .. }
        | IrOp::Ddy { modifiers, .. }
        | IrOp::Nrm { modifiers, .. }
        | IrOp::Lit { modifiers, .. }
        | IrOp::SinCos { modifiers, .. }
        | IrOp::Min { modifiers, .. }
        | IrOp::Max { modifiers, .. }
        | IrOp::SetCmp { modifiers, .. }
        | IrOp::Select { modifiers, .. }
        | IrOp::Pow { modifiers, .. }
        | IrOp::TexSample { modifiers, .. } => modifiers,
    }
}

pub fn generate_wgsl(ir: &crate::sm3::ir::ShaderIr) -> Result<WgslOutput, WgslError> {
    generate_wgsl_with_options(ir, WgslOptions::default())
}

pub fn generate_wgsl_with_options(
    ir: &crate::sm3::ir::ShaderIr,
    options: WgslOptions,
) -> Result<WgslOutput, WgslError> {
    // Collect usage so we can declare required locals and constant defs.
    let mut usage = RegUsage::new();
    collect_reg_usage(&ir.body, &mut usage, 0)?;
    for body in ir.subroutines.values() {
        collect_reg_usage(body, &mut usage, 0)?;
    }

    // Hostile-input hardening: decoding already caps indices using `crate::shader_limits`, but
    // keep a second line of defense here since WGSL codegen can otherwise balloon into large output
    // shaders.
    let max_used_index = usage
        .temps
        .iter()
        .chain(&usage.addrs)
        .chain(&usage.predicates)
        .chain(&usage.float_consts)
        .chain(&usage.int_consts)
        .chain(&usage.bool_consts)
        .chain(&usage.misc_inputs)
        .copied()
        .max()
        .unwrap_or(0)
        .max(usage.inputs.iter().map(|(_, idx)| *idx).max().unwrap_or(0))
        .max(
            usage
                .outputs_used
                .iter()
                .map(|(_, idx)| *idx)
                .max()
                .unwrap_or(0),
        );
    if max_used_index > MAX_D3D9_SHADER_REGISTER_INDEX {
        return Err(err(format!(
            "register index {max_used_index} exceeds maximum {MAX_D3D9_SHADER_REGISTER_INDEX}"
        )));
    }
    if let Some(&max_samp) = usage.samplers.iter().max() {
        if max_samp > MAX_D3D9_SAMPLER_REGISTER_INDEX {
            return Err(err(format!(
                "sampler index s{max_samp} exceeds maximum s{MAX_D3D9_SAMPLER_REGISTER_INDEX}"
            )));
        }
    }

    if ir.version.stage == ShaderStage::Vertex && !usage.misc_inputs.is_empty() {
        return Err(err(
            "MISCTYPE (misc#) registers are not supported in vertex shader WGSL lowering",
        ));
    }

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

    let subroutine_infos = build_subroutine_info_map(ir)?;

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

    let depth_out_regs: BTreeSet<u32> = usage
        .outputs_used
        .iter()
        .filter_map(|(file, idx)| (*file == RegFile::DepthOut).then_some(*idx))
        .chain(
            ir.outputs
                .iter()
                .filter_map(|decl| (decl.reg.file == RegFile::DepthOut).then_some(decl.reg.index)),
        )
        .collect();

    let mut wgsl = String::new();

    // Shader constants: pack per-stage register files into a single uniform buffer to keep
    // bindings stable across shader stages (VS=0..255, PS=256..511).
    // Uniform buffers use std140-like layout rules: arrays have a minimum 16-byte stride even for
    // scalar elements. Store the bool bank as `vec4<u32>` (4 bools per element) to keep a tight
    // 2048-byte layout (512 u32 values) while remaining WGSL-valid.
    wgsl.push_str(
        "struct Constants { c: array<vec4<f32>, 512>, i: array<vec4<i32>, 512>, b: array<vec4<u32>, 128>, };\n",
    );
    wgsl.push_str("@group(0) @binding(0) var<uniform> constants: Constants;\n\n");

    let sampler_group = sampler_bind_group(ir.version.stage);

    let sampler_type_map: HashMap<u32, TextureType> = ir
        .samplers
        .iter()
        .map(|decl| (decl.index, decl.texture_type))
        .collect();

    let mut sampler_bindings = HashMap::new();
    let mut sampler_texture_types = HashMap::new();
    for s in &usage.samplers {
        let declared_ty = sampler_type_map
            .get(s)
            .copied()
            .unwrap_or(TextureType::Texture2D);
        let binding_ty = if declared_ty == TextureType::Texture1D {
            TextureType::Texture2D
        } else {
            declared_ty
        };
        let wgsl_tex_ty = wgsl_texture_type(binding_ty)?;
        // Binding contract matches the legacy token-stream translator and the AeroGPU executor:
        //   texture binding = 2*s
        //   sampler binding = 2*s + 1
        let tex_binding = s * 2;
        let samp_binding = tex_binding + 1;
        sampler_bindings.insert(*s, (tex_binding, samp_binding));
        sampler_texture_types.insert(*s, binding_ty);
        let _ = writeln!(
            wgsl,
            "@group({sampler_group}) @binding({tex_binding}) var tex{s}: {wgsl_tex_ty};"
        );
        let _ = writeln!(
            wgsl,
            "@group({sampler_group}) @binding({samp_binding}) var samp{s}: sampler;"
        );
    }
    if !usage.samplers.is_empty() {
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
    let _ = writeln!(wgsl, "const CONST_BASE: u32 = {}u;\n", const_base);

    // Embedded float constants (`def c#`). These behave like constant-register writes that occur
    // before shader execution and must override the uniform constant buffer even when accessed via
    // relative indexing (`cN[a0.x]`).
    //
    // Declare them as module-scope `const` so that helper functions (used for relative indexing)
    // can reference them without capturing function-local variables.
    if !f32_defs.is_empty() {
        for (idx, value) in &f32_defs {
            let _ = writeln!(
                wgsl,
                "const c{idx}: vec4<f32> = vec4<f32>({}, {}, {}, {});",
                format_f32(value[0]),
                format_f32(value[1]),
                format_f32(value[2]),
                format_f32(value[3])
            );
        }
        wgsl.push('\n');

        // Helper for relative constant addressing that applies `def` overrides without inflating
        // WGSL size linearly with `(#relative accesses * #defs)`.
        wgsl.push_str("fn aero_read_const(idx_in: u32) -> vec4<f32> {\n");
        wgsl.push_str("  let idx: u32 = min(idx_in, 255u);\n");
        for def_idx in f32_defs.keys() {
            let _ = writeln!(wgsl, "  if (idx == {def_idx}u) {{ return c{def_idx}; }}");
        }
        wgsl.push_str("  return constants.c[CONST_BASE + idx];\n");
        wgsl.push_str("}\n\n");
    }

    // Private register state shared between the entry point and SM3 subroutine helper functions.
    //
    // WGSL functions cannot capture entry-point locals. Declaring registers as module-scope
    // `var<private>` allows helper functions (`call` targets) to access the same register state as
    // the main program.
    for idx in &usage.int_consts {
        if let Some(value) = i32_defs.get(idx).copied() {
            let _ = writeln!(
                wgsl,
                "const i{idx}: vec4<i32> = vec4<i32>({}, {}, {}, {});",
                value[0], value[1], value[2], value[3]
            );
        } else {
            let _ = writeln!(wgsl, "var<private> i{idx}: vec4<i32> = vec4<i32>(0);");
        }
    }
    for idx in &usage.bool_consts {
        if let Some(v) = bool_defs.get(idx).copied() {
            let _ = writeln!(
                wgsl,
                "const b{idx}: vec4<bool> = vec4<bool>({v}, {v}, {v}, {v});"
            );
        } else {
            let _ = writeln!(wgsl, "var<private> b{idx}: vec4<bool> = vec4<bool>(false);");
        }
    }
    if let Some(max_r) = usage.temps.iter().copied().max() {
        for r in 0..=max_r {
            let _ = writeln!(wgsl, "var<private> r{r}: vec4<f32> = vec4<f32>(0.0);");
        }
    }
    if let Some(max_a) = usage.addrs.iter().copied().max() {
        for a in 0..=max_a {
            let _ = writeln!(wgsl, "var<private> a{a}: vec4<i32> = vec4<i32>(0);");
        }
    }
    for l in &usage.loop_regs {
        let reg = RegRef {
            file: RegFile::Loop,
            index: *l,
            relative: None,
        };
        let name = reg_var_name(&reg)?;
        let _ = writeln!(wgsl, "var<private> {name}: vec4<i32> = vec4<i32>(0);");
    }
    if let Some(max_p) = usage.predicates.iter().copied().max() {
        for p in 0..=max_p {
            let _ = writeln!(wgsl, "var<private> p{p}: vec4<bool> = vec4<bool>(false);");
        }
    }
    for (file, index) in &usage.inputs {
        if !matches!(*file, RegFile::Input | RegFile::Texture) {
            continue;
        }
        let reg = RegRef {
            file: *file,
            index: *index,
            relative: None,
        };
        let name = reg_var_name(&reg)?;
        let _ = writeln!(wgsl, "var<private> {name}: vec4<f32> = vec4<f32>(0.0);");
    }
    for idx in &usage.misc_inputs {
        let _ = writeln!(wgsl, "var<private> misc{idx}: vec4<f32> = vec4<f32>(0.0);");
    }
    if !usage.temps.is_empty()
        || !usage.addrs.is_empty()
        || !usage.int_consts.is_empty()
        || !usage.bool_consts.is_empty()
        || !usage.loop_regs.is_empty()
        || !usage.predicates.is_empty()
        || !usage.inputs.is_empty()
        || !usage.misc_inputs.is_empty()
    {
        wgsl.push('\n');
    }

    match ir.version.stage {
        ShaderStage::Vertex => {
            if !usage.misc_inputs.is_empty() {
                return Err(err(
                    "MiscType (vPos/vFace) inputs are only supported in pixel shaders",
                ));
            }
            if !depth_out_regs.is_empty() {
                return Err(err(format!(
                    "DepthOut (oDepth) is only valid in pixel shaders, but appears in a vertex shader (indices: {depth_out_regs:?})"
                )));
            }

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
            for (file, index) in &usage.outputs_written {
                if matches!(
                    *file,
                    RegFile::AttrOut | RegFile::TexCoordOut | RegFile::Output
                ) {
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

            // Outputs used by the shader. These are mutable private vars that get copied into the
            // return value at the end.
            let mut required_outputs = usage.outputs_used.clone();
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
                    "var<private> {name}: {} = {};",
                    ty.wgsl_vec4(),
                    default_vec4(ty)
                );
            }
            wgsl.push('\n');

            // SM3 subroutines (`label` targets).
            for (label, body) in &ir.subroutines {
                let _ = writeln!(wgsl, "fn aero_sub_l{label}() {{");
                let mut state = EmitState::new(true);
                emit_block(
                    &mut wgsl,
                    body,
                    1,
                    0,
                    ShaderStage::Vertex,
                    &f32_defs,
                    &sampler_type_map,
                    &subroutine_infos,
                    &mut state,
                )?;
                wgsl.push_str("}\n\n");
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

            // Bind vertex inputs to private regs (`v#`).
            if has_inputs {
                for idx in &vs_inputs {
                    let _ = writeln!(wgsl, "  v{idx} = input.v{idx};");
                }
            }

            // Load uniform-provided integer/bool constants into private registers so SM3 call
            // targets can read them. Embedded `defi` / `defb` constants are emitted as module-scope
            // `const` and do not require initialization here.
            for idx in &usage.int_consts {
                if !i32_defs.contains_key(idx) {
                    let _ = writeln!(wgsl, "  i{idx} = constants.i[CONST_BASE + {idx}u];");
                }
            }
            for idx in &usage.bool_consts {
                if !bool_defs.contains_key(idx) {
                    // Bool constants are stored packed as `vec4<u32>` in the uniform buffer
                    // (4 scalar bools per element) to satisfy WGSL uniform layout rules while
                    // remaining compact.
                    let vec_idx = (const_base / 4u32) + (*idx / 4u32);
                    let comp = match *idx % 4u32 {
                        0 => "x",
                        1 => "y",
                        2 => "z",
                        _ => "w",
                    };
                    let _ = writeln!(
                        wgsl,
                        "  b{idx} = vec4<bool>(constants.b[{vec_idx}u].{comp} != 0u);"
                    );
                }
            }

            wgsl.push('\n');
            let mut state = EmitState::new(false);
            emit_block(
                &mut wgsl,
                &ir.body,
                1,
                0,
                ShaderStage::Vertex,
                &f32_defs,
                &sampler_type_map,
                &subroutine_infos,
                &mut state,
            )?;

            wgsl.push_str("  var out: VsOut;\n");
            wgsl.push_str("  out.pos = oPos;\n");
            if options.half_pixel_center {
                wgsl.push_str("  out.pos.x = out.pos.x - half_pixel.inv_viewport.x * out.pos.w;\n");
                wgsl.push_str("  out.pos.y = out.pos.y + half_pixel.inv_viewport.y * out.pos.w;\n");
            }
            for &(file, index) in vs_varying_locations.keys() {
                let reg = RegRef {
                    file,
                    index,
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

            // Some D3D9 pixel shader inputs are really system values, not inter-stage varyings.
            // fxc can emit e.g. `dcl_position v0` and then read `v0` instead of using the
            // dedicated `vPos` misc register file. Treat POSITION inputs as aliases for `vPos`,
            // which maps to WGSL `@builtin(position)` in fragment stage.
            let mut ps_position_inputs = BTreeSet::<u32>::new();
            for (file, index) in &ps_inputs {
                if *file != RegFile::Input {
                    continue;
                }
                let semantic = input_semantics.get(&(*file, *index));
                if matches!(
                    semantic,
                    Some(Semantic::Position(_)) | Some(Semantic::PositionT(_))
                ) {
                    ps_position_inputs.insert(*index);
                }
            }

            // Builtin inputs (misc register file).
            let mut needs_frag_pos = !ps_position_inputs.is_empty();
            let mut needs_front_facing = false;
            for idx in &usage.misc_inputs {
                match *idx {
                    0 => needs_frag_pos = true,
                    1 => needs_front_facing = true,
                    _ => {
                        return Err(err(format!(
                            "unsupported MiscType input misc{idx} (only misc0=vPos and misc1=vFace are supported)"
                        )));
                    }
                }
            }

            let has_inputs = !ps_inputs.is_empty() || needs_frag_pos || needs_front_facing;

            if depth_out_regs.len() > 1 || depth_out_regs.iter().any(|&idx| idx != 0) {
                return Err(err(format!(
                    "pixel shader uses DepthOut registers {depth_out_regs:?}; only oDepth (index 0) is supported"
                )));
            }
            let has_depth_out = !depth_out_regs.is_empty();

            let mut ps_input_locations: BTreeMap<(RegFile, u32), u32> = BTreeMap::new();
            let mut loc_to_reg: BTreeMap<u32, (RegFile, u32)> = BTreeMap::new();
            for (file, index) in &ps_inputs {
                let semantic = input_semantics.get(&(*file, *index));
                if *file == RegFile::Input
                    && matches!(
                        semantic,
                        Some(Semantic::Position(_)) | Some(Semantic::PositionT(_))
                    )
                {
                    // POSITION input is mapped via `@builtin(position)`, not a location.
                    continue;
                }
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
            for (file, index) in &usage.outputs_written {
                if *file == RegFile::ColorOut {
                    color_outputs.insert(*index);
                }
            }
            color_outputs.insert(0);

            wgsl.push_str("struct FsOut {\n");
            for idx in &color_outputs {
                let _ = writeln!(wgsl, "  @location({idx}) oC{idx}: vec4<f32>,");
            }
            if has_depth_out {
                wgsl.push_str("  @builtin(frag_depth) depth: f32,\n");
            }
            wgsl.push_str("};\n\n");

            // Outputs used by the shader. These are mutable private vars that get copied into the
            // return value at the end.
            let mut required_outputs = usage.outputs_used.clone();
            required_outputs.extend(color_outputs.iter().map(|&idx| (RegFile::ColorOut, idx)));
            if has_depth_out {
                required_outputs.insert((RegFile::DepthOut, 0));
            }
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
                    "var<private> {name}: {} = {};",
                    ty.wgsl_vec4(),
                    default_vec4(ty)
                );
            }
            wgsl.push('\n');
            // Guard used to suppress `discard` during speculative subroutine execution.
            wgsl.push_str("var<private> _aero_call_guard: bool = true;\n\n");

            // SM3 subroutines (`label` targets).
            for (label, body) in &ir.subroutines {
                let _ = writeln!(wgsl, "fn aero_sub_l{label}() {{");
                let mut state = EmitState::new(true);
                emit_block(
                    &mut wgsl,
                    body,
                    1,
                    0,
                    ShaderStage::Pixel,
                    &f32_defs,
                    &sampler_type_map,
                    &subroutine_infos,
                    &mut state,
                )?;
                wgsl.push_str("}\n\n");
            }

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
                if needs_frag_pos {
                    wgsl.push_str("  @builtin(position) frag_pos: vec4<f32>,\n");
                }
                if needs_front_facing {
                    wgsl.push_str("  @builtin(front_facing) front_facing: bool,\n");
                }
                wgsl.push_str("};\n\n");
                wgsl.push_str("@fragment\nfn fs_main(input: FsIn) -> FsOut {\n");
            } else {
                // WGSL does not permit empty structs, so if the shader uses no varyings we omit the
                // input parameter entirely.
                wgsl.push_str("@fragment\nfn fs_main() -> FsOut {\n");
            }

            // Bind pixel inputs to private regs (`v#` / `t#` / misc#).
            if has_inputs {
                for (file, index) in &ps_inputs {
                    if *file == RegFile::Input && ps_position_inputs.contains(index) {
                        let _ = writeln!(wgsl, "  v{index} = input.frag_pos;");
                        continue;
                    }
                    let reg = RegRef {
                        file: *file,
                        index: *index,
                        relative: None,
                    };
                    let name = reg_var_name(&reg)?;
                    let _ = writeln!(wgsl, "  {name} = input.{name};");
                }

                // Builtin inputs (misc register file).
                if usage.misc_inputs.contains(&0) {
                    wgsl.push_str("  misc0 = input.frag_pos;\n");
                }
                if usage.misc_inputs.contains(&1) {
                    // D3D9 vFace is a float sign (+1 or -1). WGSL exposes front-facing as a
                    // boolean, so map it to the legacy sign convention and splat to vec4.
                    wgsl.push_str("  let face: f32 = select(-1.0, 1.0, input.front_facing);\n");
                    wgsl.push_str("  misc1 = vec4<f32>(face, face, face, face);\n");
                }
            }

            // Load uniform-provided integer/bool constants into private registers so SM3 call
            // targets can read them. Embedded `defi` / `defb` constants are emitted as module-scope
            // `const` and do not require initialization here.
            for idx in &usage.int_consts {
                if !i32_defs.contains_key(idx) {
                    let _ = writeln!(wgsl, "  i{idx} = constants.i[CONST_BASE + {idx}u];");
                }
            }
            for idx in &usage.bool_consts {
                if !bool_defs.contains_key(idx) {
                    // Bool constants are stored packed as `vec4<u32>` in the uniform buffer
                    // (4 scalar bools per element) to satisfy WGSL uniform layout rules while
                    // remaining compact.
                    let vec_idx = (const_base / 4u32) + (*idx / 4u32);
                    let comp = match *idx % 4u32 {
                        0 => "x",
                        1 => "y",
                        2 => "z",
                        _ => "w",
                    };
                    let _ = writeln!(
                        wgsl,
                        "  b{idx} = vec4<bool>(constants.b[{vec_idx}u].{comp} != 0u);"
                    );
                }
            }

            wgsl.push('\n');
            let mut state = EmitState::new(false);
            emit_block(
                &mut wgsl,
                &ir.body,
                1,
                0,
                ShaderStage::Pixel,
                &f32_defs,
                &sampler_type_map,
                &subroutine_infos,
                &mut state,
            )?;

            wgsl.push_str("  var out: FsOut;\n");
            for idx in &color_outputs {
                let _ = writeln!(wgsl, "  out.oC{idx} = oC{idx};");
            }
            if has_depth_out {
                wgsl.push_str("  out.depth = oDepth.x;\n");
            }
            wgsl.push_str("  return out;\n}\n");
        }
    }

    if wgsl.len() > MAX_D3D9_WGSL_BYTES {
        return Err(err(format!(
            "generated WGSL size {} exceeds maximum {MAX_D3D9_WGSL_BYTES} bytes",
            wgsl.len()
        )));
    }

    Ok(WgslOutput {
        wgsl,
        entry_point,
        bind_group_layout: BindGroupLayout {
            sampler_group,
            sampler_bindings,
            sampler_texture_types,
        },
    })
}
