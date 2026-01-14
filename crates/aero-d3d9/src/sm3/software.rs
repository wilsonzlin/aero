//! Minimal software implementation of the SM2/3 (D3D9) programmable pipeline.
//!
//! This is a reference interpreter for the structured `sm3::ir::ShaderIr` used by the
//! new shader pipeline. It is intended for deterministic, headless regression tests
//! (pixel hash comparisons) on CI where a GPU/WebGPU adapter may be missing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::software::{RenderTarget, Texture, Vec4};
use crate::state::{
    BlendFactor, BlendOp, BlendState, SamplerState, VertexDecl, VertexElementType, VertexUsage,
};
use crate::vertex::{AdaptiveLocationMap, DeclUsage, VertexLocationMap};

use super::wgsl::varying_location;
use crate::shader_limits::{MAX_D3D9_SHADER_CONTROL_FLOW_NESTING, MAX_D3D9_SHADER_REGISTER_INDEX};
use crate::sm3::decode::{
    ResultShift, SrcModifier, Swizzle, SwizzleComponent, TextureType, WriteMask,
};
use crate::sm3::ir::{
    Block, CompareOp, Cond, Dst, InstModifiers, IrOp, PredicateRef, RegFile, RegRef, Semantic, Src,
    Stmt, TexSampleKind,
};

const MAX_LOOP_ITERS: usize = 1_024;
const CONST_INT_REGS: usize = MAX_D3D9_SHADER_REGISTER_INDEX as usize + 1;
const CONST_BOOL_REGS: usize = MAX_D3D9_SHADER_REGISTER_INDEX as usize + 1;

fn semantic_to_decl_usage(semantic: &Semantic) -> Option<(DeclUsage, u8)> {
    let (usage, index) = match semantic {
        Semantic::Position(i) => (DeclUsage::Position, *i),
        Semantic::BlendWeight(i) => (DeclUsage::BlendWeight, *i),
        Semantic::BlendIndices(i) => (DeclUsage::BlendIndices, *i),
        Semantic::Normal(i) => (DeclUsage::Normal, *i),
        Semantic::Tangent(i) => (DeclUsage::Tangent, *i),
        Semantic::Binormal(i) => (DeclUsage::Binormal, *i),
        Semantic::Color(i) => (DeclUsage::Color, *i),
        Semantic::TexCoord(i) => (DeclUsage::TexCoord, *i),
        Semantic::PositionT(i) => (DeclUsage::PositionT, *i),
        Semantic::PointSize(i) => (DeclUsage::PSize, *i),
        Semantic::Fog(i) => (DeclUsage::Fog, *i),
        Semantic::Depth(i) => (DeclUsage::Depth, *i),
        Semantic::Sample(i) => (DeclUsage::Sample, *i),
        Semantic::TessFactor(i) => (DeclUsage::TessFactor, *i),
        Semantic::Other { usage, index } => {
            let usage = DeclUsage::from_u8(*usage).ok()?;
            return Some((usage, *index));
        }
    };
    Some((usage, index))
}

#[derive(Debug, Clone)]
struct ConstBank {
    f32: [Vec4; 256],
    i32: [Vec4; CONST_INT_REGS],
    bool: [Vec4; CONST_BOOL_REGS],
}

#[derive(Debug, Clone, Copy)]
struct PixelBuiltins {
    frag_pos: Vec4,
    front_facing: bool,
}

fn swizzle(v: Vec4, swz: Swizzle) -> Vec4 {
    let a = [v.x, v.y, v.z, v.w];
    let idx = |c: SwizzleComponent| match c {
        SwizzleComponent::X => a[0],
        SwizzleComponent::Y => a[1],
        SwizzleComponent::Z => a[2],
        SwizzleComponent::W => a[3],
    };
    Vec4::new(idx(swz.0[0]), idx(swz.0[1]), idx(swz.0[2]), idx(swz.0[3]))
}

fn apply_src_modifier(v: Vec4, modifier: SrcModifier) -> Vec4 {
    match modifier {
        SrcModifier::None => v,
        SrcModifier::Negate => -v,
        SrcModifier::Bias => v - Vec4::splat(0.5),
        SrcModifier::BiasNegate => -(v - Vec4::splat(0.5)),
        SrcModifier::Sign => v.mul_scalar(2.0) - Vec4::splat(1.0),
        SrcModifier::SignNegate => -(v.mul_scalar(2.0) - Vec4::splat(1.0)),
        SrcModifier::Comp => Vec4::splat(1.0) - v,
        SrcModifier::X2 => v.mul_scalar(2.0),
        SrcModifier::X2Negate => -v.mul_scalar(2.0),
        SrcModifier::Dz => {
            let z = v.z.max(f32::EPSILON);
            v.div_scalar(z)
        }
        SrcModifier::Dw => {
            let w = v.w.max(f32::EPSILON);
            v.div_scalar(w)
        }
        SrcModifier::Abs => v.abs(),
        SrcModifier::AbsNegate => -v.abs(),
        SrcModifier::Not => Vec4::splat(1.0) - v,
        SrcModifier::Unknown(_) => v,
    }
}

fn apply_result_modifier(v: Vec4, modifiers: &InstModifiers) -> Vec4 {
    let v = match modifiers.shift {
        ResultShift::None => v,
        ResultShift::Mul2 => v.mul_scalar(2.0),
        ResultShift::Mul4 => v.mul_scalar(4.0),
        ResultShift::Mul8 => v.mul_scalar(8.0),
        ResultShift::Div2 => v.mul_scalar(0.5),
        ResultShift::Div4 => v.mul_scalar(0.25),
        ResultShift::Div8 => v.mul_scalar(0.125),
        ResultShift::Unknown(_) => v,
    };
    if modifiers.saturate {
        v.clamp01()
    } else {
        v
    }
}

fn apply_write_mask(dst: &mut Vec4, mask: WriteMask, value: Vec4) {
    if mask.contains(SwizzleComponent::X) {
        dst.x = value.x;
    }
    if mask.contains(SwizzleComponent::Y) {
        dst.y = value.y;
    }
    if mask.contains(SwizzleComponent::Z) {
        dst.z = value.z;
    }
    if mask.contains(SwizzleComponent::W) {
        dst.w = value.w;
    }
}

fn component(v: Vec4, c: SwizzleComponent) -> f32 {
    match c {
        SwizzleComponent::X => v.x,
        SwizzleComponent::Y => v.y,
        SwizzleComponent::Z => v.z,
        SwizzleComponent::W => v.w,
    }
}

fn compare(op: CompareOp, a: f32, b: f32) -> bool {
    match op {
        CompareOp::Gt => a > b,
        CompareOp::Ge => a >= b,
        CompareOp::Eq => a == b,
        CompareOp::Ne => a != b,
        CompareOp::Lt => a < b,
        CompareOp::Le => a <= b,
        CompareOp::Unknown(_) => false,
    }
}

fn predicate_truth(pred: &PredicateRef, preds: &[Vec4]) -> bool {
    let idx = pred.reg.index as usize;
    let v = preds.get(idx).copied().unwrap_or(Vec4::ZERO);
    let truth = component(v, pred.component) != 0.0;
    if pred.negate {
        !truth
    } else {
        truth
    }
}

fn resolve_reg_index(
    reg: &RegRef,
    temps: &[Vec4],
    addrs: &[Vec4],
    loops: &[Vec4],
    preds: &[Vec4],
) -> Option<u32> {
    let mut idx = reg.index as i32;
    if let Some(rel) = &reg.relative {
        // Relative addressing is defined in D3D9 in terms of integer offsets.
        // The hardware behaviour is somewhat subtle; we use truncation towards
        // zero (`as i32`) as a deterministic and reasonable approximation.
        let rel_val = read_reg_raw(&rel.reg, temps, addrs, loops, preds).unwrap_or(Vec4::ZERO);
        let offset = component(rel_val, rel.component) as i32;
        idx = idx.saturating_add(offset);
    }
    if reg.file == RegFile::Const {
        // The WGSL lowering clamps relative constant indexing to the D3D9 constant register range
        // to avoid OOB array access. Match that behaviour here so software interpreter tests are
        // consistent with WGSL output.
        idx = idx.clamp(0, 255);
        Some(idx as u32)
    } else if idx < 0 {
        None
    } else {
        Some(idx as u32)
    }
}

fn read_reg_raw(
    reg: &RegRef,
    temps: &[Vec4],
    addrs: &[Vec4],
    loops: &[Vec4],
    preds: &[Vec4],
) -> Option<Vec4> {
    let idx = resolve_reg_index(reg, temps, addrs, loops, preds)?;
    let idx_usize = idx as usize;
    Some(match reg.file {
        RegFile::Temp => temps.get(idx_usize).copied().unwrap_or(Vec4::ZERO),
        RegFile::Addr => addrs.get(idx_usize).copied().unwrap_or(Vec4::ZERO),
        RegFile::Loop => loops.get(idx_usize).copied().unwrap_or(Vec4::ZERO),
        RegFile::Predicate => preds.get(idx_usize).copied().unwrap_or(Vec4::ZERO),
        _ => Vec4::ZERO,
    })
}

#[allow(clippy::too_many_arguments)]
fn exec_src(
    src: &Src,
    temps: &[Vec4],
    addrs: &[Vec4],
    loops: &[Vec4],
    preds: &[Vec4],
    inputs_v: &HashMap<u16, Vec4>,
    inputs_t: &HashMap<u16, Vec4>,
    constants: &ConstBank,
    builtins: &PixelBuiltins,
) -> Vec4 {
    let idx = resolve_reg_index(&src.reg, temps, addrs, loops, preds);
    let v = match (src.reg.file, idx) {
        (RegFile::Temp, Some(i)) => temps.get(i as usize).copied().unwrap_or(Vec4::ZERO),
        (RegFile::Addr, Some(i)) => addrs.get(i as usize).copied().unwrap_or(Vec4::ZERO),
        (RegFile::Loop, Some(i)) => loops.get(i as usize).copied().unwrap_or(Vec4::ZERO),
        (RegFile::Predicate, Some(i)) => preds.get(i as usize).copied().unwrap_or(Vec4::ZERO),
        (RegFile::Input, Some(i)) => inputs_v.get(&(i as u16)).copied().unwrap_or(Vec4::ZERO),
        (RegFile::Texture, Some(i)) => inputs_t.get(&(i as u16)).copied().unwrap_or(Vec4::ZERO),
        (RegFile::Const, Some(i)) => constants.f32.get(i as usize).copied().unwrap_or(Vec4::ZERO),
        (RegFile::ConstInt, Some(i)) => {
            constants.i32.get(i as usize).copied().unwrap_or(Vec4::ZERO)
        }
        (RegFile::ConstBool, Some(i)) => constants
            .bool
            .get(i as usize)
            .copied()
            .unwrap_or(Vec4::ZERO),
        // Pixel shader builtins.
        (RegFile::MiscType, Some(0)) => builtins.frag_pos,
        (RegFile::MiscType, Some(1)) => Vec4::splat(if builtins.front_facing { 1.0 } else { -1.0 }),
        _ => Vec4::ZERO,
    };
    let v = swizzle(v, src.swizzle);
    apply_src_modifier(v, src.modifier)
}

#[allow(clippy::too_many_arguments)]
fn exec_dst(
    dst: &Dst,
    temps: &mut [Vec4],
    addrs: &mut [Vec4],
    loops: &mut [Vec4],
    preds: &mut [Vec4],
    o_pos: &mut Vec4,
    o_attr: &mut HashMap<u16, Vec4>,
    o_tex: &mut HashMap<u16, Vec4>,
    o_out: &mut HashMap<u16, Vec4>,
    o_color: &mut Vec4,
    value: Vec4,
) {
    let idx = resolve_reg_index(&dst.reg, temps, addrs, loops, preds);
    let idx_u16 = idx.and_then(|i| u16::try_from(i).ok());
    match dst.reg.file {
        RegFile::Temp => {
            if let Some(i) = idx {
                if let Some(v) = temps.get_mut(i as usize) {
                    apply_write_mask(v, dst.mask, value);
                }
            }
        }
        RegFile::Addr => {
            if let Some(i) = idx {
                if let Some(v) = addrs.get_mut(i as usize) {
                    apply_write_mask(v, dst.mask, value);
                }
            }
        }
        RegFile::Loop => {
            if let Some(i) = idx {
                if let Some(v) = loops.get_mut(i as usize) {
                    apply_write_mask(v, dst.mask, value);
                }
            }
        }
        RegFile::Predicate => {
            if let Some(i) = idx {
                if let Some(v) = preds.get_mut(i as usize) {
                    apply_write_mask(v, dst.mask, value);
                }
            }
        }
        RegFile::RastOut => {
            apply_write_mask(o_pos, dst.mask, value);
        }
        RegFile::AttrOut => {
            if let Some(i) = idx_u16 {
                let v = o_attr.entry(i).or_insert(Vec4::ZERO);
                apply_write_mask(v, dst.mask, value);
            }
        }
        RegFile::TexCoordOut => {
            if let Some(i) = idx_u16 {
                let v = o_tex.entry(i).or_insert(Vec4::ZERO);
                apply_write_mask(v, dst.mask, value);
            }
        }
        RegFile::Output => {
            if let Some(i) = idx_u16 {
                let v = o_out.entry(i).or_insert(Vec4::ZERO);
                apply_write_mask(v, dst.mask, value);
            }
        }
        RegFile::ColorOut => {
            apply_write_mask(o_color, dst.mask, value);
        }
        RegFile::DepthOut
        | RegFile::Const
        | RegFile::ConstInt
        | RegFile::ConstBool
        | RegFile::Label
        | RegFile::MiscType
        | RegFile::Input
        | RegFile::Texture
        | RegFile::Sampler => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Break,
    Discard,
    Return,
}

#[allow(clippy::too_many_arguments)]
fn eval_cond(
    cond: &Cond,
    temps: &[Vec4],
    addrs: &[Vec4],
    loops: &[Vec4],
    preds: &[Vec4],
    inputs_v: &HashMap<u16, Vec4>,
    inputs_t: &HashMap<u16, Vec4>,
    constants: &ConstBank,
    builtins: &PixelBuiltins,
) -> bool {
    match cond {
        Cond::NonZero { src } => {
            exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            )
            .x != 0.0
        }
        Cond::Compare { op, src0, src1 } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            )
            .x;
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            )
            .x;
            compare(*op, a, b)
        }
        Cond::Predicate { pred } => predicate_truth(pred, preds),
    }
}

#[allow(clippy::too_many_arguments)]
fn exec_block(
    block: &Block,
    depth: usize,
    subroutines: &BTreeMap<u32, Block>,
    temps: &mut [Vec4],
    addrs: &mut [Vec4],
    loops: &mut [Vec4],
    preds: &mut [Vec4],
    inputs_v: &HashMap<u16, Vec4>,
    inputs_t: &HashMap<u16, Vec4>,
    constants: &ConstBank,
    builtins: &PixelBuiltins,
    sampler_types: &HashMap<u32, TextureType>,
    textures: &HashMap<u16, Texture>,
    sampler_states: &HashMap<u16, SamplerState>,
    o_pos: &mut Vec4,
    o_attr: &mut HashMap<u16, Vec4>,
    o_tex: &mut HashMap<u16, Vec4>,
    o_out: &mut HashMap<u16, Vec4>,
    o_color: &mut Vec4,
) -> Flow {
    if depth > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        return Flow::Continue;
    }

    for stmt in &block.stmts {
        let flow = match stmt {
            Stmt::Op(op) => {
                exec_op(
                    op,
                    temps,
                    addrs,
                    loops,
                    preds,
                    inputs_v,
                    inputs_t,
                    constants,
                    builtins,
                    sampler_types,
                    textures,
                    sampler_states,
                    o_pos,
                    o_attr,
                    o_tex,
                    o_out,
                    o_color,
                );
                Flow::Continue
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                let take_then = eval_cond(
                    cond, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
                );
                if take_then {
                    exec_block(
                        then_block,
                        depth + 1,
                        subroutines,
                        temps,
                        addrs,
                        loops,
                        preds,
                        inputs_v,
                        inputs_t,
                        constants,
                        builtins,
                        sampler_types,
                        textures,
                        sampler_states,
                        o_pos,
                        o_attr,
                        o_tex,
                        o_out,
                        o_color,
                    )
                } else if let Some(else_block) = else_block {
                    exec_block(
                        else_block,
                        depth + 1,
                        subroutines,
                        temps,
                        addrs,
                        loops,
                        preds,
                        inputs_v,
                        inputs_t,
                        constants,
                        builtins,
                        sampler_types,
                        textures,
                        sampler_states,
                        o_pos,
                        o_attr,
                        o_tex,
                        o_out,
                        o_color,
                    )
                } else {
                    Flow::Continue
                }
            }
            Stmt::Loop { init, body } => {
                // D3D9 SM2/3 `loop aL, i#` has a finite trip count derived from the integer constant
                // register (i#.x=start, i#.y=end, i#.z=step). A malformed shader could specify a
                // zero step, so we keep a hard cap here for safety and determinism.
                let ctrl = exec_src(
                    &Src {
                        reg: init.ctrl_reg.clone(),
                        swizzle: Swizzle::identity(),
                        modifier: SrcModifier::None,
                    },
                    temps,
                    addrs,
                    loops,
                    preds,
                    inputs_v,
                    inputs_t,
                    constants,
                    builtins,
                );
                let start = ctrl.x as i32;
                let end = ctrl.y as i32;
                let step = ctrl.z as i32;

                let Some(loop_idx) = resolve_reg_index(&init.loop_reg, temps, addrs, loops, preds)
                    .map(|v| v as usize)
                else {
                    return Flow::Continue;
                };
                if loop_idx >= loops.len() {
                    return Flow::Continue;
                }

                // Save/restore the full loop register value to emulate D3D's loop stack semantics
                // for nested loops.
                let saved_loop_reg = loops[loop_idx];
                loops[loop_idx].x = start as f32;

                let mut discarded = false;
                for _ in 0..MAX_LOOP_ITERS {
                    if step == 0 {
                        break;
                    }

                    let counter = loops[loop_idx].x as i32;
                    if (step > 0 && counter > end) || (step < 0 && counter < end) {
                        break;
                    }

                    match exec_block(
                        body,
                        depth + 1,
                        subroutines,
                        temps,
                        addrs,
                        loops,
                        preds,
                        inputs_v,
                        inputs_t,
                        constants,
                        builtins,
                        sampler_types,
                        textures,
                        sampler_states,
                        o_pos,
                        o_attr,
                        o_tex,
                        o_out,
                        o_color,
                    ) {
                        Flow::Continue => {}
                        Flow::Break => break,
                        Flow::Discard => {
                            discarded = true;
                            break;
                        }
                        Flow::Return => {
                            // Early return from inside the loop unwinds the loop stack.
                            loops[loop_idx] = saved_loop_reg;
                            return Flow::Return;
                        }
                    }

                    loops[loop_idx].x = counter.wrapping_add(step) as f32;
                }

                // Restore the loop register to the pre-loop value.
                loops[loop_idx] = saved_loop_reg;

                if discarded {
                    Flow::Discard
                } else {
                    Flow::Continue
                }
            }
            Stmt::Rep { count_reg, body } => {
                // D3D9 SM2/3 `rep i#` repeats the loop body `i#.x` times, using `aL.x` as the loop
                // counter. Keep the same safety cap as `loop` for determinism.
                let count_v = exec_src(
                    &Src {
                        reg: count_reg.clone(),
                        swizzle: Swizzle::identity(),
                        modifier: SrcModifier::None,
                    },
                    temps,
                    addrs,
                    loops,
                    preds,
                    inputs_v,
                    inputs_t,
                    constants,
                    builtins,
                );
                let count = count_v.x as i32;

                // `rep` implicitly uses `aL` (loop register 0).
                let loop_idx = 0usize;
                if loop_idx >= loops.len() {
                    return Flow::Continue;
                }

                // Save/restore loop register value to emulate nested-loop stack semantics.
                let saved_loop_reg = loops[loop_idx];
                loops[loop_idx].x = 0.0;

                let mut discarded = false;
                for _ in 0..MAX_LOOP_ITERS {
                    let counter = loops[loop_idx].x as i32;
                    if counter >= count {
                        break;
                    }

                    match exec_block(
                        body,
                        depth + 1,
                        subroutines,
                        temps,
                        addrs,
                        loops,
                        preds,
                        inputs_v,
                        inputs_t,
                        constants,
                        builtins,
                        sampler_types,
                        textures,
                        sampler_states,
                        o_pos,
                        o_attr,
                        o_tex,
                        o_out,
                        o_color,
                    ) {
                        Flow::Continue => {}
                        Flow::Break => break,
                        Flow::Discard => {
                            discarded = true;
                            break;
                        }
                        Flow::Return => {
                            loops[loop_idx] = saved_loop_reg;
                            return Flow::Return;
                        }
                    }

                    loops[loop_idx].x = counter.wrapping_add(1) as f32;
                }

                loops[loop_idx] = saved_loop_reg;

                if discarded {
                    Flow::Discard
                } else {
                    Flow::Continue
                }
            }
            Stmt::Break => Flow::Break,
            Stmt::BreakIf { cond } => {
                if eval_cond(
                    cond, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
                ) {
                    Flow::Break
                } else {
                    Flow::Continue
                }
            }
            Stmt::Discard { src } => {
                let v = exec_src(
                    src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
                );
                if v.x < 0.0 || v.y < 0.0 || v.z < 0.0 || v.w < 0.0 {
                    Flow::Discard
                } else {
                    Flow::Continue
                }
            }
            Stmt::Call { label } => {
                if let Some(body) = subroutines.get(label) {
                    match exec_block(
                        body,
                        depth,
                        subroutines,
                        temps,
                        addrs,
                        loops,
                        preds,
                        inputs_v,
                        inputs_t,
                        constants,
                        builtins,
                        sampler_types,
                        textures,
                        sampler_states,
                        o_pos,
                        o_attr,
                        o_tex,
                        o_out,
                        o_color,
                    ) {
                        Flow::Continue | Flow::Break | Flow::Return => Flow::Continue,
                        Flow::Discard => Flow::Discard,
                    }
                } else {
                    Flow::Continue
                }
            }
            Stmt::Return => Flow::Return,
        };

        match flow {
            Flow::Continue => {}
            other => return other,
        }
    }

    Flow::Continue
}

#[allow(clippy::too_many_arguments)]
fn exec_op(
    op: &IrOp,
    temps: &mut [Vec4],
    addrs: &mut [Vec4],
    loops: &mut [Vec4],
    preds: &mut [Vec4],
    inputs_v: &HashMap<u16, Vec4>,
    inputs_t: &HashMap<u16, Vec4>,
    constants: &ConstBank,
    builtins: &PixelBuiltins,
    sampler_types: &HashMap<u32, TextureType>,
    textures: &HashMap<u16, Texture>,
    sampler_states: &HashMap<u16, SamplerState>,
    o_pos: &mut Vec4,
    o_attr: &mut HashMap<u16, Vec4>,
    o_tex: &mut HashMap<u16, Vec4>,
    o_out: &mut HashMap<u16, Vec4>,
    o_color: &mut Vec4,
) {
    let (dst, modifiers) = match op {
        IrOp::Mov { dst, modifiers, .. }
        | IrOp::Mova { dst, modifiers, .. }
        | IrOp::Add { dst, modifiers, .. }
        | IrOp::Sub { dst, modifiers, .. }
        | IrOp::Mul { dst, modifiers, .. }
        | IrOp::Mad { dst, modifiers, .. }
        | IrOp::Lrp { dst, modifiers, .. }
        | IrOp::Dp2 { dst, modifiers, .. }
        | IrOp::Dp2Add { dst, modifiers, .. }
        | IrOp::Dp3 { dst, modifiers, .. }
        | IrOp::Dp4 { dst, modifiers, .. }
        | IrOp::Dst { dst, modifiers, .. }
        | IrOp::Crs { dst, modifiers, .. }
        | IrOp::MatrixMul { dst, modifiers, .. }
        | IrOp::Rcp { dst, modifiers, .. }
        | IrOp::Rsq { dst, modifiers, .. }
        | IrOp::Frc { dst, modifiers, .. }
        | IrOp::Abs { dst, modifiers, .. }
        | IrOp::Sgn { dst, modifiers, .. }
        | IrOp::Exp { dst, modifiers, .. }
        | IrOp::Log { dst, modifiers, .. }
        | IrOp::Ddx { dst, modifiers, .. }
        | IrOp::Ddy { dst, modifiers, .. }
        | IrOp::Nrm { dst, modifiers, .. }
        | IrOp::Lit { dst, modifiers, .. }
        | IrOp::SinCos { dst, modifiers, .. }
        | IrOp::Min { dst, modifiers, .. }
        | IrOp::Max { dst, modifiers, .. }
        | IrOp::SetCmp { dst, modifiers, .. }
        | IrOp::Select { dst, modifiers, .. }
        | IrOp::Pow { dst, modifiers, .. }
        | IrOp::TexSample { dst, modifiers, .. } => (dst, modifiers),
    };

    if let Some(pred) = &modifiers.predicate {
        if !predicate_truth(pred, preds) {
            return;
        }
    }

    let v = match op {
        IrOp::Mov { src, .. } => exec_src(
            src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
        ),
        IrOp::Mova { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            // Match the WGSL lowering: apply float result modifiers first, then convert float -> int.
            let a = apply_result_modifier(a, modifiers);
            let to_i32 = |v: f32| (v as i32) as f32;
            Vec4::new(to_i32(a.x), to_i32(a.y), to_i32(a.z), to_i32(a.w))
        }
        IrOp::Add { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            a + b
        }
        IrOp::Sub { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            a - b
        }
        IrOp::Mul { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            a * b
        }
        IrOp::Mad {
            src0, src1, src2, ..
        } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let c = exec_src(
                src2, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            a * b + c
        }
        IrOp::Lrp {
            src0, src1, src2, ..
        } => {
            let t = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let a = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src2, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            // D3D9 `lrp`: dst = t*a + (1-t)*b.
            t * a + (Vec4::splat(1.0) - t) * b
        }
        IrOp::Dp2 { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            Vec4::splat(a.x * b.x + a.y * b.y)
        }
        IrOp::Dp2Add {
            src0, src1, src2, ..
        } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let c = exec_src(
                src2, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            Vec4::splat(a.x * b.x + a.y * b.y + c.x)
        }
        IrOp::Dp3 { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            Vec4::splat(a.dot3(b))
        }
        IrOp::Dp4 { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            Vec4::splat(a.dot4(b))
        }
        IrOp::Dst { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            // D3D9 `dst`: x is 1.0; y is src0.y * src1.y; z is src0.z; w is src1.w.
            Vec4::new(1.0, a.y * b.y, a.z, b.w)
        }
        IrOp::Crs { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            // D3D9 `crs`: cross product of the xyz components. The W component is not well-specified,
            // but most shaders only consume `.xyz`. Set W to 1.0 for deterministic output.
            Vec4::new(
                a.y * b.z - a.z * b.y,
                a.z * b.x - a.x * b.z,
                a.x * b.y - a.y * b.x,
                1.0,
            )
        }
        IrOp::MatrixMul {
            dst,
            src0,
            src1,
            m,
            n,
            ..
        } => {
            let v = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );

            let mut dots = [0.0_f32; 4];
            for (col, dot) in dots.iter_mut().enumerate().take(usize::from(*n)) {
                let mut column = src1.clone();
                column.reg.index = column.reg.index.saturating_add(col as u32);
                let mvec = exec_src(
                    &column, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
                );
                *dot = match *m {
                    4 => v.dot4(mvec),
                    3 => v.dot3(mvec),
                    2 => v.x * mvec.x + v.y * mvec.y,
                    _ => 0.0,
                };
            }

            let raw = Vec4::new(dots[0], dots[1], dots[2], dots[3]);
            let modded = apply_result_modifier(raw, modifiers);

            let prev = match dst.reg.file {
                RegFile::Temp => resolve_reg_index(&dst.reg, temps, addrs, loops, preds)
                    .and_then(|i| temps.get(i as usize).copied())
                    .unwrap_or(Vec4::ZERO),
                RegFile::Addr => resolve_reg_index(&dst.reg, temps, addrs, loops, preds)
                    .and_then(|i| addrs.get(i as usize).copied())
                    .unwrap_or(Vec4::ZERO),
                RegFile::Loop => resolve_reg_index(&dst.reg, temps, addrs, loops, preds)
                    .and_then(|i| loops.get(i as usize).copied())
                    .unwrap_or(Vec4::ZERO),
                RegFile::Predicate => resolve_reg_index(&dst.reg, temps, addrs, loops, preds)
                    .and_then(|i| preds.get(i as usize).copied())
                    .unwrap_or(Vec4::ZERO),
                RegFile::RastOut => *o_pos,
                RegFile::AttrOut => resolve_reg_index(&dst.reg, temps, addrs, loops, preds)
                    .and_then(|i| u16::try_from(i).ok())
                    .and_then(|i| o_attr.get(&i).copied())
                    .unwrap_or(Vec4::ZERO),
                RegFile::TexCoordOut => resolve_reg_index(&dst.reg, temps, addrs, loops, preds)
                    .and_then(|i| u16::try_from(i).ok())
                    .and_then(|i| o_tex.get(&i).copied())
                    .unwrap_or(Vec4::ZERO),
                RegFile::ColorOut => *o_color,
                _ => Vec4::ZERO,
            };

            match *n {
                4 => modded,
                3 => Vec4::new(modded.x, modded.y, modded.z, prev.w),
                2 => Vec4::new(modded.x, modded.y, prev.z, prev.w),
                1 => Vec4::new(modded.x, prev.y, prev.z, prev.w),
                _ => prev,
            }
        }
        IrOp::Rcp { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            Vec4::new(1.0 / a.x, 1.0 / a.y, 1.0 / a.z, 1.0 / a.w)
        }
        IrOp::Rsq { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let inv_sqrt = |v: f32| 1.0 / v.sqrt();
            Vec4::new(inv_sqrt(a.x), inv_sqrt(a.y), inv_sqrt(a.z), inv_sqrt(a.w))
        }
        IrOp::Frc { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let fract = |v: f32| v - v.floor();
            Vec4::new(fract(a.x), fract(a.y), fract(a.z), fract(a.w))
        }
        IrOp::Abs { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            a.abs()
        }
        IrOp::Sgn { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let sign = |v: f32| {
                if v > 0.0 {
                    1.0
                } else if v < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            };
            Vec4::new(sign(a.x), sign(a.y), sign(a.z), sign(a.w))
        }
        IrOp::Exp { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let exp2 = |v: f32| 2.0_f32.powf(v);
            Vec4::new(exp2(a.x), exp2(a.y), exp2(a.z), exp2(a.w))
        }
        IrOp::Log { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let log2 = |v: f32| v.log2();
            Vec4::new(log2(a.x), log2(a.y), log2(a.z), log2(a.w))
        }
        IrOp::Ddx { .. } | IrOp::Ddy { .. } => {
            // Screen-space derivatives require neighboring pixels, which the software interpreter
            // does not currently model. Treat as zero to keep the reference path deterministic.
            Vec4::ZERO
        }
        IrOp::Nrm { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let len = (a.x * a.x + a.y * a.y + a.z * a.z).sqrt();
            if len <= f32::EPSILON {
                Vec4::new(0.0, 0.0, 0.0, 1.0)
            } else {
                Vec4::new(a.x / len, a.y / len, a.z / len, 1.0)
            }
        }
        IrOp::Lit { src, .. } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let y = a.x.max(0.0);
            let z = if a.x > 0.0 {
                a.y.max(0.0).powf(a.w)
            } else {
                0.0
            };
            Vec4::new(1.0, y, z, 1.0)
        }
        IrOp::SinCos {
            src, src1, src2, ..
        } => {
            let a = exec_src(
                src, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let angle = match (src1, src2) {
                (None, None) => a.x,
                (Some(src1), Some(src2)) => {
                    let s1 = exec_src(
                        src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
                    );
                    let s2 = exec_src(
                        src2, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
                    );
                    a.x * s1.x + s2.x
                }
                _ => a.x,
            };
            Vec4::new(angle.sin(), angle.cos(), 0.0, 0.0)
        }
        IrOp::Min { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            Vec4::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z), a.w.min(b.w))
        }
        IrOp::Max { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            Vec4::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z), a.w.max(b.w))
        }
        IrOp::SetCmp { op, src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let cmp = |av: f32, bv: f32| compare(*op, av, bv) as u8 as f32;
            Vec4::new(cmp(a.x, b.x), cmp(a.y, b.y), cmp(a.z, b.z), cmp(a.w, b.w))
        }
        IrOp::Select {
            cond,
            src_ge,
            src_lt,
            ..
        } => {
            let cond = exec_src(
                cond, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let ge = exec_src(
                src_ge, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let lt = exec_src(
                src_lt, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let pick = |cond: f32, ge: f32, lt: f32| if cond >= 0.0 { ge } else { lt };
            Vec4::new(
                pick(cond.x, ge.x, lt.x),
                pick(cond.y, ge.y, lt.y),
                pick(cond.z, ge.z, lt.z),
                pick(cond.w, ge.w, lt.w),
            )
        }
        IrOp::Pow { src0, src1, .. } => {
            let a = exec_src(
                src0, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let b = exec_src(
                src1, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let pow = |a: f32, b: f32| a.powf(b);
            Vec4::new(pow(a.x, b.x), pow(a.y, b.y), pow(a.z, b.z), pow(a.w, b.w))
        }
        IrOp::TexSample {
            kind,
            coord,
            ddx: _,
            ddy: _,
            sampler,
            ..
        } => {
            let coord_v = exec_src(
                coord, temps, addrs, loops, preds, inputs_v, inputs_t, constants, builtins,
            );
            let sampler_u16 = u16::try_from(*sampler).ok();

            if let Some(s) = sampler_u16 {
                let samp = sampler_states.get(&s).copied().unwrap_or_default();
                let ty = sampler_types
                    .get(sampler)
                    .copied()
                    .unwrap_or(TextureType::Texture2D);

                match ty {
                    TextureType::Texture2D => {
                        let (u, v) = match kind {
                            TexSampleKind::ImplicitLod { project: true } => {
                                let w = coord_v.w.max(f32::EPSILON);
                                (coord_v.x / w, coord_v.y / w)
                            }
                            TexSampleKind::ImplicitLod { project: false }
                            | TexSampleKind::Bias
                            | TexSampleKind::ExplicitLod
                            | TexSampleKind::Grad => (coord_v.x, coord_v.y),
                        };
                        match textures.get(&s) {
                            Some(Texture::Texture2D(tex)) => tex.sample(samp, (u, v)),
                            _ => Vec4::ZERO,
                        }
                    }
                    TextureType::TextureCube => {
                        let (x, y, z) = match kind {
                            TexSampleKind::ImplicitLod { project: true } => {
                                let w = coord_v.w.max(f32::EPSILON);
                                (coord_v.x / w, coord_v.y / w, coord_v.z / w)
                            }
                            TexSampleKind::ImplicitLod { project: false }
                            | TexSampleKind::Bias
                            | TexSampleKind::ExplicitLod
                            | TexSampleKind::Grad => (coord_v.x, coord_v.y, coord_v.z),
                        };
                        match textures.get(&s) {
                            Some(Texture::TextureCube(tex)) => tex.sample(samp, (x, y, z)),
                            _ => Vec4::ZERO,
                        }
                    }
                    _ => Vec4::ZERO,
                }
            } else {
                Vec4::ZERO
            }
        }
    };

    let v = if matches!(op, IrOp::Mova { .. } | IrOp::MatrixMul { .. }) {
        v
    } else {
        apply_result_modifier(v, modifiers)
    };
    exec_dst(
        dst, temps, addrs, loops, preds, o_pos, o_attr, o_tex, o_out, o_color, v,
    );
}

fn blend_factor(factor: BlendFactor, src: Vec4, dst: Vec4) -> Vec4 {
    match factor {
        BlendFactor::Zero => Vec4::splat(0.0),
        BlendFactor::One => Vec4::splat(1.0),
        BlendFactor::SrcColor => src,
        BlendFactor::OneMinusSrcColor => Vec4::splat(1.0) - src,
        BlendFactor::SrcAlpha => Vec4::splat(src.w),
        BlendFactor::OneMinusSrcAlpha => Vec4::splat(1.0 - src.w),
        BlendFactor::DstColor => dst,
        BlendFactor::OneMinusDstColor => Vec4::splat(1.0) - dst,
        BlendFactor::DstAlpha => Vec4::splat(dst.w),
        BlendFactor::OneMinusDstAlpha => Vec4::splat(1.0 - dst.w),
    }
}

fn blend(state: BlendState, src: Vec4, dst: Vec4) -> Vec4 {
    if !state.enabled {
        return src;
    }
    let sf = blend_factor(state.src_factor, src, dst);
    let df = blend_factor(state.dst_factor, src, dst);
    let s = src * sf;
    let d = dst * df;
    match state.op {
        BlendOp::Add => s + d,
        BlendOp::Subtract => s - d,
        BlendOp::ReverseSubtract => d - s,
    }
}

fn read_f32(bytes: &[u8]) -> f32 {
    f32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_vertex_element(bytes: &[u8], ty: VertexElementType) -> Vec4 {
    match ty {
        VertexElementType::Float1 => Vec4::new(read_f32(&bytes[0..4]), 0.0, 0.0, 1.0),
        VertexElementType::Float2 => {
            Vec4::new(read_f32(&bytes[0..4]), read_f32(&bytes[4..8]), 0.0, 1.0)
        }
        VertexElementType::Float3 => Vec4::new(
            read_f32(&bytes[0..4]),
            read_f32(&bytes[4..8]),
            read_f32(&bytes[8..12]),
            1.0,
        ),
        VertexElementType::Float4 => Vec4::new(
            read_f32(&bytes[0..4]),
            read_f32(&bytes[4..8]),
            read_f32(&bytes[8..12]),
            read_f32(&bytes[12..16]),
        ),
        VertexElementType::Color => {
            // D3DCOLOR is BGRA8.
            let b = bytes[0] as f32 / 255.0;
            let g = bytes[1] as f32 / 255.0;
            let r = bytes[2] as f32 / 255.0;
            let a = bytes[3] as f32 / 255.0;
            Vec4::new(r, g, b, a)
        }
    }
}

#[derive(Debug, Clone)]
struct VsOut {
    clip_pos: Vec4,
    attr: HashMap<u16, Vec4>,
    tex: HashMap<u16, Vec4>,
    out: HashMap<u16, Vec4>,
}

fn prepare_constants(constants_in: &[Vec4; 256], ir: &crate::sm3::ir::ShaderIr) -> ConstBank {
    let mut f32 = *constants_in;
    for def in &ir.const_defs_f32 {
        if let Some(slot) = f32.get_mut(def.index as usize) {
            *slot = Vec4::new(def.value[0], def.value[1], def.value[2], def.value[3]);
        }
    }

    let mut i32 = [Vec4::ZERO; CONST_INT_REGS];
    for def in &ir.const_defs_i32 {
        if let Some(slot) = i32.get_mut(def.index as usize) {
            *slot = Vec4::new(
                def.value[0] as f32,
                def.value[1] as f32,
                def.value[2] as f32,
                def.value[3] as f32,
            );
        }
    }

    let mut bool = [Vec4::ZERO; CONST_BOOL_REGS];
    for def in &ir.const_defs_bool {
        if let Some(slot) = bool.get_mut(def.index as usize) {
            let v = def.value as u8 as f32;
            *slot = Vec4::splat(v);
        }
    }

    ConstBank { f32, i32, bool }
}

fn run_vertex_shader(
    ir: &crate::sm3::ir::ShaderIr,
    inputs: &HashMap<u16, Vec4>,
    constants: &ConstBank,
    sampler_types: &HashMap<u32, TextureType>,
    textures: &HashMap<u16, Texture>,
    sampler_states: &HashMap<u16, SamplerState>,
) -> VsOut {
    let mut temps = vec![Vec4::ZERO; 32];
    let mut addrs = vec![Vec4::ZERO; 4];
    let mut loops = vec![Vec4::ZERO; 4];
    let mut preds = vec![Vec4::ZERO; 16];

    let mut o_pos = Vec4::ZERO;
    let mut o_attr = HashMap::<u16, Vec4>::new();
    let mut o_tex = HashMap::<u16, Vec4>::new();
    let mut o_out = HashMap::<u16, Vec4>::new();
    let mut dummy_color = Vec4::ZERO;
    let empty_t = HashMap::new();
    let builtins = PixelBuiltins {
        frag_pos: Vec4::ZERO,
        front_facing: true,
    };

    exec_block(
        &ir.body,
        0,
        &ir.subroutines,
        &mut temps,
        &mut addrs,
        &mut loops,
        &mut preds,
        inputs,
        &empty_t,
        constants,
        &builtins,
        sampler_types,
        textures,
        sampler_states,
        &mut o_pos,
        &mut o_attr,
        &mut o_tex,
        &mut o_out,
        &mut dummy_color,
    );

    VsOut {
        clip_pos: o_pos,
        attr: o_attr,
        tex: o_tex,
        out: o_out,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_pixel_shader(
    ir: &crate::sm3::ir::ShaderIr,
    inputs_v: &HashMap<u16, Vec4>,
    inputs_t: &HashMap<u16, Vec4>,
    constants: &ConstBank,
    builtins: &PixelBuiltins,
    sampler_types: &HashMap<u32, TextureType>,
    textures: &HashMap<u16, Texture>,
    sampler_states: &HashMap<u16, SamplerState>,
) -> Option<Vec4> {
    let mut temps = vec![Vec4::ZERO; 32];
    let mut addrs = vec![Vec4::ZERO; 4];
    let mut loops = vec![Vec4::ZERO; 4];
    let mut preds = vec![Vec4::ZERO; 16];

    let mut dummy_pos = Vec4::ZERO;
    let mut dummy_attr = HashMap::<u16, Vec4>::new();
    let mut dummy_tex = HashMap::<u16, Vec4>::new();
    let mut dummy_out = HashMap::<u16, Vec4>::new();
    let mut o_color = Vec4::ZERO;

    let flow = exec_block(
        &ir.body,
        0,
        &ir.subroutines,
        &mut temps,
        &mut addrs,
        &mut loops,
        &mut preds,
        inputs_v,
        inputs_t,
        constants,
        builtins,
        sampler_types,
        textures,
        sampler_states,
        &mut dummy_pos,
        &mut dummy_attr,
        &mut dummy_tex,
        &mut dummy_out,
        &mut o_color,
    );

    match flow {
        Flow::Discard => None,
        Flow::Continue | Flow::Break | Flow::Return => Some(o_color),
    }
}

#[derive(Debug, Clone)]
struct ScreenVertex {
    x: f32,
    y: f32,
    ndc_z: f32,
    inv_w: f32,
    varyings: HashMap<u32, Vec4>,
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

pub struct DrawParams<'a> {
    pub vs: &'a crate::sm3::ir::ShaderIr,
    pub ps: &'a crate::sm3::ir::ShaderIr,
    pub vertex_decl: &'a VertexDecl,
    pub vertex_buffer: &'a [u8],
    pub indices: Option<&'a [u16]>,
    pub constants: &'a [Vec4; 256],
    pub textures: &'a HashMap<u16, Texture>,
    pub sampler_states: &'a HashMap<u16, SamplerState>,
    pub blend_state: BlendState,
}

#[derive(Debug, Clone, Copy)]
struct PsInputLocation {
    file: RegFile,
    index: u16,
    location: u32,
}

struct PixelContext<'a> {
    ps: &'a crate::sm3::ir::ShaderIr,
    constants: ConstBank,
    sampler_types: HashMap<u32, TextureType>,
    textures: &'a HashMap<u16, Texture>,
    sampler_states: &'a HashMap<u16, SamplerState>,
    blend_state: BlendState,
    ps_inputs: Vec<PsInputLocation>,
    ps_position_inputs: BTreeSet<u16>,
}

fn collect_used_pixel_inputs(block: &Block, out: &mut BTreeSet<(RegFile, u32)>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Op(op) => collect_used_pixel_inputs_op(op, out),
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                collect_used_pixel_inputs_cond(cond, out);
                collect_used_pixel_inputs(then_block, out);
                if let Some(else_block) = else_block {
                    collect_used_pixel_inputs(else_block, out);
                }
            }
            Stmt::Loop { init, body } => {
                collect_used_pixel_inputs_reg(&init.loop_reg, out);
                collect_used_pixel_inputs_reg(&init.ctrl_reg, out);
                collect_used_pixel_inputs(body, out);
            }
            Stmt::Rep { count_reg, body } => {
                collect_used_pixel_inputs_reg(count_reg, out);
                collect_used_pixel_inputs(body, out);
            }
            Stmt::Break => {}
            Stmt::BreakIf { cond } => collect_used_pixel_inputs_cond(cond, out),
            Stmt::Discard { src } => collect_used_pixel_inputs_src(src, out),
            Stmt::Call { .. } | Stmt::Return => {}
        }
    }
}

#[deny(unreachable_patterns)]
fn collect_used_pixel_inputs_op(op: &IrOp, out: &mut BTreeSet<(RegFile, u32)>) {
    match op {
        IrOp::Mov { src, modifiers, .. }
        | IrOp::Mova { src, modifiers, .. }
        | IrOp::Rcp { src, modifiers, .. }
        | IrOp::Rsq { src, modifiers, .. }
        | IrOp::Frc { src, modifiers, .. }
        | IrOp::Abs { src, modifiers, .. }
        | IrOp::Sgn { src, modifiers, .. }
        | IrOp::Exp { src, modifiers, .. }
        | IrOp::Log { src, modifiers, .. }
        | IrOp::Ddx { src, modifiers, .. }
        | IrOp::Ddy { src, modifiers, .. }
        | IrOp::Nrm { src, modifiers, .. }
        | IrOp::Lit { src, modifiers, .. } => {
            collect_used_pixel_inputs_src(src, out);
            collect_used_pixel_inputs_modifiers(modifiers, out);
        }
        IrOp::SinCos {
            src,
            src1,
            src2,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src(src, out);
            if let Some(src1) = src1 {
                collect_used_pixel_inputs_src(src1, out);
            }
            if let Some(src2) = src2 {
                collect_used_pixel_inputs_src(src2, out);
            }
            collect_used_pixel_inputs_modifiers(modifiers, out);
        }
        IrOp::Add {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Sub {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Mul {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Min {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Max {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Dp2 {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Dp3 {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Dp4 {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Dst {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Crs {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::SetCmp {
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Pow {
            src0,
            src1,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src(src0, out);
            collect_used_pixel_inputs_src(src1, out);
            collect_used_pixel_inputs_modifiers(modifiers, out);
        }
        IrOp::MatrixMul {
            src0,
            src1,
            n,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src(src0, out);
            // Matrix helper ops implicitly read `src1 + column_index` for 0..n.
            for col in 0..*n {
                let mut column = src1.clone();
                if let Some(idx) = column.reg.index.checked_add(u32::from(col)) {
                    column.reg.index = idx;
                }
                collect_used_pixel_inputs_src(&column, out);
            }
            collect_used_pixel_inputs_modifiers(modifiers, out);
        }
        IrOp::Select {
            cond,
            src_ge,
            src_lt,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src(cond, out);
            collect_used_pixel_inputs_src(src_ge, out);
            collect_used_pixel_inputs_src(src_lt, out);
            collect_used_pixel_inputs_modifiers(modifiers, out);
        }
        // Ops with 3 source operands (mad/lrp/dp2add).
        //
        // Keep these as separate match arms: this function is built with
        // `#[deny(unreachable_patterns)]` and merge conflict resolutions have repeatedly duplicated
        // `Dp2Add` inside an or-pattern group, breaking the build.
        IrOp::Dp2Add {
            src0,
            src1,
            src2,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src3(src0, src1, src2, modifiers, out);
        }
        IrOp::Mad {
            src0,
            src1,
            src2,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src3(src0, src1, src2, modifiers, out);
        }
        IrOp::Lrp {
            src0,
            src1,
            src2,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src3(src0, src1, src2, modifiers, out);
        }
        IrOp::TexSample {
            coord,
            ddx,
            ddy,
            modifiers,
            ..
        } => {
            collect_used_pixel_inputs_src(coord, out);
            if let Some(ddx) = ddx {
                collect_used_pixel_inputs_src(ddx, out);
            }
            if let Some(ddy) = ddy {
                collect_used_pixel_inputs_src(ddy, out);
            }
            collect_used_pixel_inputs_modifiers(modifiers, out);
        }
    }
}

fn collect_used_pixel_inputs_cond(cond: &Cond, out: &mut BTreeSet<(RegFile, u32)>) {
    match cond {
        Cond::NonZero { src } => collect_used_pixel_inputs_src(src, out),
        Cond::Compare { src0, src1, .. } => {
            collect_used_pixel_inputs_src(src0, out);
            collect_used_pixel_inputs_src(src1, out);
        }
        Cond::Predicate { pred } => collect_used_pixel_inputs_reg(&pred.reg, out),
    }
}

fn collect_used_pixel_inputs_src(src: &Src, out: &mut BTreeSet<(RegFile, u32)>) {
    collect_used_pixel_inputs_reg(&src.reg, out);
}

fn collect_used_pixel_inputs_src3(
    src0: &Src,
    src1: &Src,
    src2: &Src,
    modifiers: &InstModifiers,
    out: &mut BTreeSet<(RegFile, u32)>,
) {
    collect_used_pixel_inputs_src(src0, out);
    collect_used_pixel_inputs_src(src1, out);
    collect_used_pixel_inputs_src(src2, out);
    collect_used_pixel_inputs_modifiers(modifiers, out);
}

fn collect_used_pixel_inputs_modifiers(
    modifiers: &InstModifiers,
    out: &mut BTreeSet<(RegFile, u32)>,
) {
    if let Some(pred) = &modifiers.predicate {
        collect_used_pixel_inputs_reg(&pred.reg, out);
    }
}

fn collect_used_pixel_inputs_reg(reg: &RegRef, out: &mut BTreeSet<(RegFile, u32)>) {
    if matches!(reg.file, RegFile::Input | RegFile::Texture) {
        out.insert((reg.file, reg.index));
    }
    if let Some(rel) = &reg.relative {
        collect_used_pixel_inputs_reg(&rel.reg, out);
    }
}

/// Draw a triangle list using SM2/3 bytecode lowered to `sm3::ir::ShaderIr`.
#[allow(clippy::too_many_arguments)]
pub fn draw(target: &mut RenderTarget, params: DrawParams<'_>) {
    let DrawParams {
        vs,
        ps,
        vertex_decl,
        vertex_buffer,
        indices,
        constants,
        textures,
        sampler_states,
        blend_state,
    } = params;

    let vs_constants = prepare_constants(constants, vs);
    let ps_constants = prepare_constants(constants, ps);
    let vs_sampler_types: HashMap<u32, TextureType> = vs
        .samplers
        .iter()
        .map(|s| (s.index, s.texture_type))
        .collect();
    let ps_sampler_types: HashMap<u32, TextureType> = ps
        .samplers
        .iter()
        .map(|s| (s.index, s.texture_type))
        .collect();

    let mut vs_output_semantics = HashMap::<u32, Semantic>::new();
    for decl in &vs.outputs {
        if decl.reg.file == RegFile::Output {
            vs_output_semantics.insert(decl.reg.index, decl.semantic.clone());
        }
    }

    let mut ps_input_semantics = HashMap::<u32, Semantic>::new();
    for decl in &ps.inputs {
        if decl.reg.file == RegFile::Input {
            ps_input_semantics.insert(decl.reg.index, decl.semantic.clone());
        }
    }

    let mut used_ps_inputs = BTreeSet::<(RegFile, u32)>::new();
    collect_used_pixel_inputs(&ps.body, &mut used_ps_inputs);
    for body in ps.subroutines.values() {
        collect_used_pixel_inputs(body, &mut used_ps_inputs);
    }
    let mut ps_inputs = Vec::<PsInputLocation>::new();
    let mut ps_position_inputs = BTreeSet::<u16>::new();
    for (file, index) in used_ps_inputs {
        let Ok(index_u16) = u16::try_from(index) else {
            continue;
        };
        let semantic = match file {
            RegFile::Input => ps_input_semantics.get(&index),
            RegFile::Texture => None,
            other => panic!("unexpected pixel shader input register file {other:?}"),
        };
        if file == RegFile::Input
            && semantic.is_some_and(|semantic| {
                matches!(semantic, Semantic::Position(_) | Semantic::PositionT(_))
            })
        {
            ps_position_inputs.insert(index_u16);
            continue;
        }
        let location = varying_location(file, index, semantic)
            .unwrap_or_else(|e| panic!("failed to map {file:?}{index} to varying location: {e}"));
        ps_inputs.push(PsInputLocation {
            file,
            index: index_u16,
            location,
        });
    }

    // Vertex shader IR may canonicalize `v#` indices to WGSL `@location(n)` values based on DCL
    // semantics. Mirror that mapping here so the software interpreter sees the same inputs as the
    // GPU path.
    let semantic_location_map = if vs.uses_semantic_locations {
        let semantics = vs
            .inputs
            .iter()
            .filter(|d| d.reg.file == RegFile::Input)
            .filter_map(|d| semantic_to_decl_usage(&d.semantic))
            .collect::<Vec<_>>();
        AdaptiveLocationMap::new(semantics).ok()
    } else {
        None
    };

    let input_keys: Vec<Option<u16>> = vertex_decl
        .elements
        .iter()
        .enumerate()
        .map(|(slot, element)| {
            if !vs.uses_semantic_locations {
                return Some(slot as u16);
            }
            let Some(map) = &semantic_location_map else {
                // Fall back to element-order locations if the semantic map cannot be constructed.
                return Some(slot as u16);
            };
            let usage = match element.usage {
                VertexUsage::Position => DeclUsage::Position,
                VertexUsage::TexCoord => DeclUsage::TexCoord,
                VertexUsage::Color => DeclUsage::Color,
            };
            let Ok(loc) = map.location_for(usage, element.usage_index) else {
                // Skip declaration elements that the shader doesn't declare.
                return None;
            };
            u16::try_from(loc).ok()
        })
        .collect();

    let fetch_vertex = |vertex_index: u32| -> HashMap<u16, Vec4> {
        let base = vertex_index as usize * vertex_decl.stride as usize;
        let mut inputs = HashMap::<u16, Vec4>::new();
        for (key, element) in input_keys.iter().copied().zip(&vertex_decl.elements) {
            let Some(key) = key else {
                continue;
            };
            let off = base + element.offset as usize;
            let bytes = &vertex_buffer[off..off + element.ty.byte_size()];
            inputs.insert(key, read_vertex_element(bytes, element.ty));
        }
        inputs
    };

    let mut verts = Vec::<ScreenVertex>::new();
    let mut emit_vertex = |vertex_index: u32| {
        let inputs = fetch_vertex(vertex_index);
        let VsOut {
            clip_pos: cp,
            attr,
            tex,
            out,
        } = run_vertex_shader(
            vs,
            &inputs,
            &vs_constants,
            &vs_sampler_types,
            textures,
            sampler_states,
        );
        let inv_w = 1.0 / cp.w.max(f32::EPSILON);
        let ndc_x = cp.x * inv_w;
        let ndc_y = cp.y * inv_w;
        let ndc_z = cp.z * inv_w;
        let sx = (ndc_x * 0.5 + 0.5) * target.width as f32;
        let sy = (-ndc_y * 0.5 + 0.5) * target.height as f32;
        let mut varyings = HashMap::<u32, Vec4>::new();
        for (index, value) in attr {
            let index = u32::from(index);
            let loc = varying_location(RegFile::AttrOut, index, None).unwrap_or_else(|e| {
                panic!("failed to map AttrOut{index} to varying location: {e}")
            });
            varyings.insert(loc, value);
        }
        for (index, value) in tex {
            let index = u32::from(index);
            let loc = varying_location(RegFile::TexCoordOut, index, None).unwrap_or_else(|e| {
                panic!("failed to map TexCoordOut{index} to varying location: {e}")
            });
            varyings.insert(loc, value);
        }
        for (index, value) in out {
            let index = u32::from(index);
            let semantic = vs_output_semantics.get(&index);
            let loc = varying_location(RegFile::Output, index, semantic)
                .unwrap_or_else(|e| panic!("failed to map Output{index} to varying location: {e}"));
            varyings.insert(loc, value);
        }
        verts.push(ScreenVertex {
            x: sx,
            y: sy,
            ndc_z,
            inv_w,
            varyings,
        });
    };

    let ctx = PixelContext {
        ps,
        constants: ps_constants,
        sampler_types: ps_sampler_types,
        textures,
        sampler_states,
        blend_state,
        ps_inputs,
        ps_position_inputs,
    };

    match indices {
        Some(idx) => {
            let max = idx.iter().copied().max().unwrap_or(0) as u32;
            for i in 0..=max {
                emit_vertex(i);
            }

            for tri in idx.chunks_exact(3) {
                let a = &verts[tri[0] as usize];
                let b = &verts[tri[1] as usize];
                let c = &verts[tri[2] as usize];
                rasterize_triangle(target, &ctx, a, b, c);
            }
        }
        None => {
            let vertex_count = (vertex_buffer.len() / vertex_decl.stride as usize) as u32;
            for i in 0..vertex_count {
                emit_vertex(i);
            }
            for tri in (0..vertex_count).collect::<Vec<_>>().chunks_exact(3) {
                let a = &verts[tri[0] as usize];
                let b = &verts[tri[1] as usize];
                let c = &verts[tri[2] as usize];
                rasterize_triangle(target, &ctx, a, b, c);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn src_input(index: u32) -> Src {
        Src {
            reg: RegRef {
                file: RegFile::Input,
                index,
                relative: None,
            },
            swizzle: Swizzle::identity(),
            modifier: SrcModifier::None,
        }
    }

    fn src_temp(index: u32) -> Src {
        Src {
            reg: RegRef {
                file: RegFile::Temp,
                index,
                relative: None,
            },
            swizzle: Swizzle::identity(),
            modifier: SrcModifier::None,
        }
    }

    #[test]
    fn collect_used_pixel_inputs_includes_dp2add_src2() {
        // Regression test: `dp2add` reads a third source operand (`src2.x`) and the software
        // raster path needs to treat it like other 3-src ops when discovering which pixel inputs
        // are required for interpolation.
        let op = IrOp::Dp2Add {
            dst: Dst {
                reg: RegRef {
                    file: RegFile::Temp,
                    index: 0,
                    relative: None,
                },
                mask: WriteMask::all(),
            },
            src0: src_input(0),
            src1: src_input(1),
            src2: src_input(2),
            modifiers: InstModifiers::none(),
        };

        let mut used = BTreeSet::new();
        collect_used_pixel_inputs_op(&op, &mut used);
        assert!(used.contains(&(RegFile::Input, 0)));
        assert!(used.contains(&(RegFile::Input, 1)));
        assert!(used.contains(&(RegFile::Input, 2)));
    }

    #[test]
    fn collect_used_pixel_inputs_includes_dp2add_predicate() {
        // Regression test: ensure the dp2add path still visits instruction modifiers.
        let op = IrOp::Dp2Add {
            dst: Dst {
                reg: RegRef {
                    file: RegFile::Temp,
                    index: 0,
                    relative: None,
                },
                mask: WriteMask::all(),
            },
            src0: src_temp(0),
            src1: src_temp(1),
            src2: src_temp(2),
            modifiers: InstModifiers {
                predicate: Some(PredicateRef {
                    reg: RegRef {
                        file: RegFile::Input,
                        index: 7,
                        relative: None,
                    },
                    component: SwizzleComponent::X,
                    negate: false,
                }),
                ..InstModifiers::none()
            },
        };

        let mut used = BTreeSet::new();
        collect_used_pixel_inputs_op(&op, &mut used);
        assert_eq!(used.len(), 1);
        assert!(used.contains(&(RegFile::Input, 7)));
    }

    #[test]
    fn collect_used_pixel_inputs_includes_mad_src2() {
        // Similar to dp2add: `mad` has three source operands.
        let op = IrOp::Mad {
            dst: Dst {
                reg: RegRef {
                    file: RegFile::Temp,
                    index: 0,
                    relative: None,
                },
                mask: WriteMask::all(),
            },
            src0: src_input(0),
            src1: src_input(1),
            src2: src_input(2),
            modifiers: InstModifiers::none(),
        };

        let mut used = BTreeSet::new();
        collect_used_pixel_inputs_op(&op, &mut used);
        assert!(used.contains(&(RegFile::Input, 0)));
        assert!(used.contains(&(RegFile::Input, 1)));
        assert!(used.contains(&(RegFile::Input, 2)));
    }

    #[test]
    fn collect_used_pixel_inputs_includes_lrp_predicate() {
        // Ensure lrp still visits instruction modifiers.
        let op = IrOp::Lrp {
            dst: Dst {
                reg: RegRef {
                    file: RegFile::Temp,
                    index: 0,
                    relative: None,
                },
                mask: WriteMask::all(),
            },
            src0: src_temp(0),
            src1: src_temp(1),
            src2: src_temp(2),
            modifiers: InstModifiers {
                predicate: Some(PredicateRef {
                    reg: RegRef {
                        file: RegFile::Input,
                        index: 7,
                        relative: None,
                    },
                    component: SwizzleComponent::X,
                    negate: false,
                }),
                ..InstModifiers::none()
            },
        };

        let mut used = BTreeSet::new();
        collect_used_pixel_inputs_op(&op, &mut used);
        assert_eq!(used.len(), 1);
        assert!(used.contains(&(RegFile::Input, 7)));
    }

    #[test]
    fn exec_dp2add_matches_sm3_definition() {
        let op = IrOp::Dp2Add {
            dst: Dst {
                reg: RegRef {
                    file: RegFile::Temp,
                    index: 3,
                    relative: None,
                },
                mask: WriteMask::all(),
            },
            src0: src_temp(0),
            src1: src_temp(1),
            src2: src_temp(2),
            modifiers: InstModifiers::none(),
        };

        let mut temps = vec![
            Vec4::new(1.0, 2.0, 0.0, 0.0),
            Vec4::new(10.0, 20.0, 0.0, 0.0),
            Vec4::new(5.0, 0.0, 0.0, 0.0),
            Vec4::ZERO,
        ];
        let mut addrs = vec![Vec4::ZERO; 1];
        let mut loops = vec![Vec4::ZERO; 1];
        let mut preds = vec![Vec4::ZERO; 1];
        let inputs_v = HashMap::<u16, Vec4>::new();
        let inputs_t = HashMap::<u16, Vec4>::new();
        let sampler_types = HashMap::<u32, TextureType>::new();
        let textures = HashMap::<u16, Texture>::new();
        let sampler_states = HashMap::<u16, SamplerState>::new();
        let constants = ConstBank {
            f32: [Vec4::ZERO; 256],
            i32: [Vec4::ZERO; CONST_INT_REGS],
            bool: [Vec4::ZERO; CONST_BOOL_REGS],
        };
        let builtins = PixelBuiltins {
            frag_pos: Vec4::ZERO,
            front_facing: true,
        };

        let mut o_pos = Vec4::ZERO;
        let mut o_attr = HashMap::<u16, Vec4>::new();
        let mut o_tex = HashMap::<u16, Vec4>::new();
        let mut o_out = HashMap::<u16, Vec4>::new();
        let mut o_color = Vec4::ZERO;

        exec_op(
            &op,
            &mut temps,
            &mut addrs,
            &mut loops,
            &mut preds,
            &inputs_v,
            &inputs_t,
            &constants,
            &builtins,
            &sampler_types,
            &textures,
            &sampler_states,
            &mut o_pos,
            &mut o_attr,
            &mut o_tex,
            &mut o_out,
            &mut o_color,
        );

        // dp2add: dot(src0.xy, src1.xy) + src2.x
        assert_eq!(temps[3], Vec4::splat(55.0));
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangle(
    target: &mut RenderTarget,
    ctx: &PixelContext<'_>,
    a: &ScreenVertex,
    b: &ScreenVertex,
    c: &ScreenVertex,
) {
    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
    let max_x = a.x.max(b.x).max(c.x).ceil().min(target.width as f32 - 1.0) as i32;
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
    let max_y = a.y.max(b.y).max(c.y).ceil().min(target.height as f32 - 1.0) as i32;

    let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
    if area.abs() < f32::EPSILON {
        return;
    }
    // D3D9 `vFace` maps to a +1/-1 sign for front/back faces. Match the default D3D9 convention
    // where clockwise-wound triangles are front-facing.
    //
    // Note: our `area` is computed with `edge(a, b, c) = cross(c-a, b-a)`, so its sign is the
    // opposite of the more common `cross(b-a, c-a)` formulation.
    let front_facing = area < 0.0;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let w0 = edge(b.x, b.y, c.x, c.y, px, py);
            let w1 = edge(c.x, c.y, a.x, a.y, px, py);
            let w2 = edge(a.x, a.y, b.x, b.y, px, py);

            if (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0) {
                let b0 = w0 / area;
                let b1 = w1 / area;
                let b2 = w2 / area;

                let inv_w = a.inv_w * b0 + b.inv_w * b1 + c.inv_w * b2;
                let inv_w = inv_w.max(f32::EPSILON);
                let w = 1.0 / inv_w;
                let ndc_z = a.ndc_z * b0 + b.ndc_z * b1 + c.ndc_z * b2;

                let interp_map = |map_a: &HashMap<u32, Vec4>,
                                  map_b: &HashMap<u32, Vec4>,
                                  map_c: &HashMap<u32, Vec4>| {
                    let mut keys = map_a.keys().copied().collect::<Vec<_>>();
                    keys.extend(map_b.keys().copied());
                    keys.extend(map_c.keys().copied());
                    keys.sort_unstable();
                    keys.dedup();

                    let mut out = HashMap::<u32, Vec4>::new();
                    for k in keys {
                        let va = map_a
                            .get(&k)
                            .copied()
                            .unwrap_or(Vec4::ZERO)
                            .mul_scalar(a.inv_w);
                        let vb = map_b
                            .get(&k)
                            .copied()
                            .unwrap_or(Vec4::ZERO)
                            .mul_scalar(b.inv_w);
                        let vc = map_c
                            .get(&k)
                            .copied()
                            .unwrap_or(Vec4::ZERO)
                            .mul_scalar(c.inv_w);
                        let v = (va.mul_scalar(b0) + vb.mul_scalar(b1) + vc.mul_scalar(b2))
                            .mul_scalar(w);
                        out.insert(k, v);
                    }
                    out
                };

                let varyings = interp_map(&a.varyings, &b.varyings, &c.varyings);

                let mut inputs_v = HashMap::<u16, Vec4>::new();
                let mut inputs_t = HashMap::<u16, Vec4>::new();
                for input in &ctx.ps_inputs {
                    let value = varyings.get(&input.location).copied().unwrap_or(Vec4::ZERO);
                    match input.file {
                        RegFile::Input => {
                            inputs_v.insert(input.index, value);
                        }
                        RegFile::Texture => {
                            inputs_t.insert(input.index, value);
                        }
                        _ => {}
                    }
                }

                let frag_pos = Vec4::new(px, py, ndc_z, inv_w);
                for idx in &ctx.ps_position_inputs {
                    inputs_v.insert(*idx, frag_pos);
                }
                let builtins = PixelBuiltins {
                    frag_pos,
                    front_facing,
                };

                if let Some(color) = run_pixel_shader(
                    ctx.ps,
                    &inputs_v,
                    &inputs_t,
                    &ctx.constants,
                    &builtins,
                    &ctx.sampler_types,
                    ctx.textures,
                    ctx.sampler_states,
                ) {
                    let dst = target.get(x as u32, y as u32);
                    let out = blend(ctx.blend_state, color, dst).clamp01();
                    target.set(x as u32, y as u32, out);
                }
            }
        }
    }
}
