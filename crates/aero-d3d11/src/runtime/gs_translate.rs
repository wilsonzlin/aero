//! Geometry shader (GS) -> WGSL compute translation.
//!
//! WebGPU does not expose geometry shaders. For bring-up we emulate a small subset of SM4 geometry
//! shaders by translating the decoded [`crate::sm4_ir::Sm4Module`] into a WGSL compute shader that
//! performs a "geometry prepass":
//! - Executes the GS instruction stream per input primitive.
//! - Expands `emit`/`cut` output into a list index buffer, performing strip→list conversion for
//!   `linestrip`/`trianglestrip` output topologies with correct restart (`cut`) semantics.
//! - Writes a `DrawIndexedIndirectArgs` struct so the render pass can consume the expanded
//!   geometry via `draw_indexed_indirect`.
//!
//! Note: this module is only **partially wired** into the AeroGPU command executor today:
//! - `CREATE_SHADER_DXBC` attempts to translate SM4 GS DXBC into a WGSL compute prepass.
//! - `DRAW` on `PointList` can execute the translated compute prepass (when translation succeeded).
//!
//! Most GS/HS/DS draws still route through the built-in compute-prepass WGSL shaders
//! (`GEOMETRY_PREPASS_CS_WGSL` / `GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL`) used for bring-up and for
//! topologies/draw paths where translated GS DXBC execution is not implemented yet.
//!
//! Note: if a GS DXBC blob cannot be translated by this module, the shader handle is still created,
//! but draws with that GS bound currently return a clear error (there is not yet a “run synthetic
//! expansion anyway” fallback for arbitrary untranslatable GS bytecode).
//!
//! The initial implementation is intentionally minimal and focuses on the instructions/operands
//! required by the in-tree GS tests (float ALU ops such as `mov`/`movc`/`add`/`mul`/`mad`/`dp3`/`dp4`
//!/`min`/`max`, plus immediate constants + `v#[]` inputs + `emit`/`cut` on stream 0).

use core::fmt;
use std::collections::HashMap;

use crate::sm4::ShaderStage;
use crate::sm4_ir::{
    GsInputPrimitive, GsOutputTopology, OperandModifier, RegFile, RegisterRef, Sm4CmpOp, Sm4Decl,
    Sm4Inst, Sm4Module, Sm4TestBool, SrcKind, Swizzle, WriteMask,
};

// D3D system-value IDs used by `Sm4Decl::InputSiv`.
// Values match the tokenized shader format (`d3d10tokenizedprogramformat.h` / `d3d11tokenizedprogramformat.h`)
// and Aero's own signature-driven translator (`shader_translate.rs`).
const D3D_NAME_PRIMITIVE_ID: u32 = 7;
const D3D_NAME_GS_INSTANCE_ID: u32 = 11;

#[derive(Clone, Copy, Debug)]
struct InputSivInfo {
    sys_value: u32,
    mask: WriteMask,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GsTranslateError {
    NotGeometryStage(ShaderStage),
    MissingDecl(&'static str),
    UnsupportedInputPrimitive {
        primitive: u32,
    },
    UnsupportedOutputTopology {
        topology: u32,
    },
    UnsupportedStream {
        inst_index: usize,
        opcode: &'static str,
        stream: u8,
    },
    InvalidGsInputVertexIndex {
        inst_index: usize,
        vertex: u32,
        verts_per_primitive: u32,
    },
    UnsupportedOperand {
        inst_index: usize,
        opcode: &'static str,
        msg: String,
    },
    UnsupportedInstruction {
        inst_index: usize,
        opcode: &'static str,
    },
    MalformedControlFlow {
        inst_index: usize,
        expected: String,
        found: String,
    },
}

impl fmt::Display for GsTranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GsTranslateError::NotGeometryStage(stage) => {
                write!(f, "GS translate: expected geometry shader module, got {stage:?}")
            }
            GsTranslateError::MissingDecl(name) => {
                write!(f, "GS translate: missing required declaration {name}")
            }
            GsTranslateError::UnsupportedInputPrimitive { primitive } => write!(
                f,
                "GS translate: unsupported input primitive {primitive} (dcl_inputprimitive)"
            ),
            GsTranslateError::UnsupportedOutputTopology { topology } => write!(
                f,
                "GS translate: unsupported output topology {topology} (dcl_outputtopology)"
            ),
            GsTranslateError::UnsupportedStream {
                inst_index,
                opcode,
                stream,
            } => write!(
                f,
                "GS translate: unsupported {opcode} stream {stream} at instruction index {inst_index} (only stream 0 is supported)"
            ),
            GsTranslateError::InvalidGsInputVertexIndex {
                inst_index,
                vertex,
                verts_per_primitive,
            } => write!(
                f,
                "GS translate: v#[] operand uses vertex index {vertex} at instruction index {inst_index}, but input primitive has only {verts_per_primitive} vertices"
            ),
            GsTranslateError::UnsupportedOperand {
                inst_index,
                opcode,
                msg,
            } => write!(
                f,
                "GS translate: unsupported operand for {opcode} at instruction index {inst_index}: {msg}"
            ),
            GsTranslateError::UnsupportedInstruction { inst_index, opcode } => write!(
                f,
                "GS translate: unsupported instruction {opcode} at index {inst_index}"
            ),
            GsTranslateError::MalformedControlFlow {
                inst_index,
                expected,
                found,
            } => write!(
                f,
                "GS translate: malformed structured control flow at instruction index {inst_index}: expected {expected}, found {found}"
            ),
        }
    }
}

impl std::error::Error for GsTranslateError {}

fn opcode_name(inst: &Sm4Inst) -> &'static str {
    match inst {
        Sm4Inst::If { .. } => "if",
        Sm4Inst::IfC { .. } => "ifc",
        Sm4Inst::Else => "else",
        Sm4Inst::EndIf => "endif",
        Sm4Inst::Loop => "loop",
        Sm4Inst::EndLoop => "endloop",
        Sm4Inst::BreakC { .. } => "breakc",
        Sm4Inst::ContinueC { .. } => "continuec",
        Sm4Inst::Break => "break",
        Sm4Inst::Continue => "continue",
        Sm4Inst::Switch { .. } => "switch",
        Sm4Inst::Case { .. } => "case",
        Sm4Inst::Default => "default",
        Sm4Inst::EndSwitch => "endswitch",
        Sm4Inst::Mov { .. } => "mov",
        Sm4Inst::Movc { .. } => "movc",
        Sm4Inst::Itof { .. } => "itof",
        Sm4Inst::Utof { .. } => "utof",
        Sm4Inst::Ftoi { .. } => "ftoi",
        Sm4Inst::Ftou { .. } => "ftou",
        Sm4Inst::F32ToF16 { .. } => "f32tof16",
        Sm4Inst::F16ToF32 { .. } => "f16tof32",
        Sm4Inst::And { .. } => "and",
        Sm4Inst::Add { .. } => "add",
        Sm4Inst::IAddC { .. } => "iaddc",
        Sm4Inst::UAddC { .. } => "uaddc",
        Sm4Inst::ISubC { .. } => "isubc",
        Sm4Inst::USubB { .. } => "usubb",
        Sm4Inst::Mul { .. } => "mul",
        Sm4Inst::Mad { .. } => "mad",
        Sm4Inst::Dp3 { .. } => "dp3",
        Sm4Inst::Dp4 { .. } => "dp4",
        Sm4Inst::Min { .. } => "min",
        Sm4Inst::Max { .. } => "max",
        Sm4Inst::IMin { .. } => "imin",
        Sm4Inst::IMax { .. } => "imax",
        Sm4Inst::UMin { .. } => "umin",
        Sm4Inst::UMax { .. } => "umax",
        Sm4Inst::IAbs { .. } => "iabs",
        Sm4Inst::INeg { .. } => "ineg",
        Sm4Inst::UDiv { .. } => "udiv",
        Sm4Inst::IDiv { .. } => "idiv",
        Sm4Inst::Rcp { .. } => "rcp",
        Sm4Inst::Rsq { .. } => "rsq",
        Sm4Inst::Bfi { .. } => "bfi",
        Sm4Inst::Ubfe { .. } => "ubfe",
        Sm4Inst::Ibfe { .. } => "ibfe",
        Sm4Inst::Cmp { .. } => "cmp",
        Sm4Inst::Bfrev { .. } => "bfrev",
        Sm4Inst::CountBits { .. } => "countbits",
        Sm4Inst::Sample { .. } => "sample",
        Sm4Inst::SampleL { .. } => "sample_l",
        Sm4Inst::Ld { .. } => "ld",
        Sm4Inst::LdRaw { .. } => "ld_raw",
        Sm4Inst::StoreRaw { .. } => "store_raw",
        Sm4Inst::LdStructured { .. } => "ld_structured",
        Sm4Inst::StoreStructured { .. } => "store_structured",
        Sm4Inst::Sync { .. } => "sync",
        Sm4Inst::Emit { .. } => "emit",
        Sm4Inst::Cut { .. } => "cut",
        Sm4Inst::EmitThenCut { .. } => "emitthen_cut",
        Sm4Inst::Ret => "ret",
        Sm4Inst::Unknown { .. } => "unknown",
        _ => "unsupported",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputTopologyKind {
    PointList,
    LineStrip,
    TriangleStrip,
}

fn input_primitive_token(prim: GsInputPrimitive) -> u32 {
    match prim {
        GsInputPrimitive::Point(v)
        | GsInputPrimitive::Line(v)
        | GsInputPrimitive::Triangle(v)
        | GsInputPrimitive::LineAdjacency(v)
        | GsInputPrimitive::TriangleAdjacency(v)
        | GsInputPrimitive::Unknown(v) => v,
    }
}

fn decode_output_topology(
    topology: GsOutputTopology,
    input_primitive: GsInputPrimitive,
) -> Result<OutputTopologyKind, GsTranslateError> {
    let input_prim_token = input_primitive_token(input_primitive);
    let likely_d3d_encoding = matches!(input_prim_token, 4 | 10 | 11 | 12 | 13);

    match topology {
        GsOutputTopology::Point(_) => Ok(OutputTopologyKind::PointList),
        GsOutputTopology::LineStrip(_) => Ok(OutputTopologyKind::LineStrip),
        // `3` is ambiguous:
        // - tokenized shader format: trianglestrip=3.
        // - D3D primitive topology enum: linestrip=3.
        //
        // Prefer the tokenized interpretation by default, but accept the D3D encoding when the
        // input primitive encoding strongly suggests the toolchain is emitting D3D topology
        // constants for GS declarations (e.g. triangle=4, triadj=12).
        GsOutputTopology::TriangleStrip(3) => {
            if likely_d3d_encoding {
                Ok(OutputTopologyKind::LineStrip)
            } else {
                Ok(OutputTopologyKind::TriangleStrip)
            }
        }
        GsOutputTopology::TriangleStrip(_) => Ok(OutputTopologyKind::TriangleStrip),
        GsOutputTopology::Unknown(other) => match other {
            1 => Ok(OutputTopologyKind::PointList),
            2 => Ok(OutputTopologyKind::LineStrip),
            3 => {
                if likely_d3d_encoding {
                    Ok(OutputTopologyKind::LineStrip)
                } else {
                    Ok(OutputTopologyKind::TriangleStrip)
                }
            }
            5 => Ok(OutputTopologyKind::TriangleStrip),
            _ => Err(GsTranslateError::UnsupportedOutputTopology { topology: other }),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GsPrepassInfo {
    /// Number of vertices in each input primitive (point=1, line=2, triangle=3, etc).
    pub input_verts_per_primitive: u32,
    /// Number of input registers (`v#[]`) referenced by the shader (1 + max register index).
    pub input_reg_count: u32,
    /// Maximum number of vertices the shader may emit per input primitive (`dcl_maxvertexcount`).
    pub max_output_vertex_count: u32,
}

#[derive(Debug, Clone)]
pub struct GsPrepassTranslation {
    pub wgsl: String,
    pub info: GsPrepassInfo,
}

/// Translate a decoded SM4 geometry shader module into a WGSL compute shader implementing the
/// geometry prepass.
///
/// The generated WGSL uses the following fixed bind group layout:
/// - `@group(0) @binding(0)` expanded vertices buffer (`ExpandedVertexBuffer`, read_write)
/// - `@group(0) @binding(1)` expanded indices buffer (`U32Buffer`, read_write)
/// - `@group(0) @binding(2)` indirect args buffer (`DrawIndexedIndirectArgs`, read_write)
/// - `@group(0) @binding(3)` atomic counters (`GsPrepassCounters`, read_write)
/// - `@group(0) @binding(4)` uniform params (`GsPrepassParams`)
/// - `@group(0) @binding(5)` GS input payload (`Vec4F32Buffer`, read)
pub fn translate_gs_module_to_wgsl_compute_prepass(
    module: &Sm4Module,
) -> Result<String, GsTranslateError> {
    Ok(translate_gs_module_to_wgsl_compute_prepass_with_entry_point(module, "cs_main")?.wgsl)
}

/// Variant of [`translate_gs_module_to_wgsl_compute_prepass`] that allows overriding the compute
/// entry point name.
pub fn translate_gs_module_to_wgsl_compute_prepass_with_entry_point(
    module: &Sm4Module,
    entry_point: &str,
) -> Result<GsPrepassTranslation, GsTranslateError> {
    if module.stage != ShaderStage::Geometry {
        return Err(GsTranslateError::NotGeometryStage(module.stage));
    }

    let mut input_primitive: Option<GsInputPrimitive> = None;
    let mut output_topology: Option<GsOutputTopology> = None;
    let mut max_output_vertices: Option<u32> = None;
    let mut gs_instance_count: Option<u32> = None;
    let mut input_sivs: HashMap<u32, InputSivInfo> = HashMap::new();
    for decl in &module.decls {
        match decl {
            Sm4Decl::GsInputPrimitive { primitive } => input_primitive = Some(*primitive),
            Sm4Decl::GsOutputTopology { topology } => output_topology = Some(*topology),
            Sm4Decl::GsMaxOutputVertexCount { max } => max_output_vertices = Some(*max),
            Sm4Decl::GsInstanceCount { count } => gs_instance_count = Some(*count),
            Sm4Decl::InputSiv {
                reg,
                mask,
                sys_value,
            } => {
                // Ignore duplicates as long as they agree on the sysvalue. Some toolchains may emit
                // redundant declarations.
                input_sivs
                    .entry(*reg)
                    .and_modify(|existing| {
                        if existing.sys_value == *sys_value {
                            existing.mask = WriteMask(existing.mask.0 | mask.0);
                        }
                    })
                    .or_insert(InputSivInfo {
                        sys_value: *sys_value,
                        mask: *mask,
                    });
            }
            _ => {}
        }
    }

    let input_primitive =
        input_primitive.ok_or(GsTranslateError::MissingDecl("dcl_inputprimitive"))?;
    let output_topology =
        output_topology.ok_or(GsTranslateError::MissingDecl("dcl_outputtopology"))?;
    let max_output_vertices =
        max_output_vertices.ok_or(GsTranslateError::MissingDecl("dcl_maxvertexcount"))?;
    let gs_instance_count = gs_instance_count.unwrap_or(1).max(1);

    let verts_per_primitive = match input_primitive {
        GsInputPrimitive::Point(_) => 1,
        GsInputPrimitive::Line(_) => 2,
        GsInputPrimitive::Triangle(_) => 3,
        GsInputPrimitive::LineAdjacency(_) => 4,
        GsInputPrimitive::TriangleAdjacency(_) => 6,
        GsInputPrimitive::Unknown(other) => {
            return Err(GsTranslateError::UnsupportedInputPrimitive { primitive: other })
        }
    };

    let output_topology_kind = decode_output_topology(output_topology, input_primitive)?;

    // Scan the instruction stream for:
    // - register indices so we can declare a local register file,
    // - v#[] usage to determine input register count,
    // - unsupported stream indices for emit/cut,
    // - out-of-range vertex indices in v#[] operands.
    let mut max_temp_reg: i32 = -1;
    let mut max_output_reg: i32 = -1;
    let mut max_gs_input_reg: i32 = -1;

    for (inst_index, inst) in module.instructions.iter().enumerate() {
        match inst {
            Sm4Inst::If { cond, .. } => {
                scan_src_operand(
                    cond,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "if",
                    &input_sivs,
                )?;
            }
            Sm4Inst::IfC { a, b, .. } => {
                scan_src_operand(
                    a,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "ifc",
                    &input_sivs,
                )?;
                scan_src_operand(
                    b,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "ifc",
                    &input_sivs,
                )?;
            }
            Sm4Inst::Else | Sm4Inst::EndIf | Sm4Inst::Loop | Sm4Inst::EndLoop => {}
            Sm4Inst::BreakC { a, b, .. } => {
                scan_src_operand(
                    a,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "breakc",
                    &input_sivs,
                )?;
                scan_src_operand(
                    b,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "breakc",
                    &input_sivs,
                )?;
            }
            Sm4Inst::ContinueC { a, b, .. } => {
                scan_src_operand(
                    a,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "continuec",
                    &input_sivs,
                )?;
                scan_src_operand(
                    b,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "continuec",
                    &input_sivs,
                )?;
            }
            Sm4Inst::Break | Sm4Inst::Continue => {}
            Sm4Inst::Switch { selector } => {
                scan_src_operand(
                    selector,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "switch",
                    &input_sivs,
                )?;
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {}
            Sm4Inst::Mov { dst, src } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    src,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "mov",
                    &input_sivs,
                )?;
            }
            Sm4Inst::Movc { dst, cond, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [cond, a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "movc",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Itof { dst, src }
            | Sm4Inst::Utof { dst, src }
            | Sm4Inst::Ftoi { dst, src }
            | Sm4Inst::Ftou { dst, src }
            | Sm4Inst::F32ToF16 { dst, src }
            | Sm4Inst::F16ToF32 { dst, src } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    src,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    opcode_name(inst),
                    &input_sivs,
                )?;
            }
            Sm4Inst::Rcp { dst, src } | Sm4Inst::Rsq { dst, src } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    src,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    opcode_name(inst),
                    &input_sivs,
                )?;
            }
            Sm4Inst::Add { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "add",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::And { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "and",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Mul { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "mul",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Mad { dst, a, b, c } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b, c] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "mad",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Min { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "min",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Max { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "max",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Dp3 { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "dp3",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Dp4 { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "dp4",
                        &input_sivs,
                    )?;
                }
            }
            Sm4Inst::Emit { stream } => {
                if *stream != 0 {
                    return Err(GsTranslateError::UnsupportedStream {
                        inst_index,
                        opcode: "emit",
                        stream: *stream,
                    });
                }
            }
            Sm4Inst::Cut { stream } => {
                if *stream != 0 {
                    return Err(GsTranslateError::UnsupportedStream {
                        inst_index,
                        opcode: "cut",
                        stream: *stream,
                    });
                }
            }
            Sm4Inst::EmitThenCut { stream } => {
                if *stream != 0 {
                    return Err(GsTranslateError::UnsupportedStream {
                        inst_index,
                        opcode: "emitthen_cut",
                        stream: *stream,
                    });
                }
            }
            Sm4Inst::Ret => {}
            other => {
                return Err(GsTranslateError::UnsupportedInstruction {
                    inst_index,
                    opcode: opcode_name(other),
                });
            }
        }
    }

    // Ensure we always declare at least o0/o1 so the expanded vertex format is well-defined even if
    // the shader never touches o1.
    max_output_reg = max_output_reg.max(1);

    let temp_reg_count = (max_temp_reg + 1).max(0) as u32;
    let output_reg_count = (max_output_reg + 1).max(0) as u32;
    let gs_input_reg_count = (max_gs_input_reg + 1).max(1) as u32;

    let mut w = WgslWriter::new();

    w.line("// ---- Aero SM4 geometry shader prepass (generated) ----");
    w.line("");

    w.line("struct ExpandedVertex {");
    w.indent();
    w.line("pos: vec4<f32>,");
    w.line("o1: vec4<f32>,");
    w.dedent();
    w.line("};");
    w.line("");

    w.line("struct ExpandedVertexBuffer { data: array<ExpandedVertex> };");
    w.line("struct U32Buffer { data: array<u32> };");
    w.line("struct Vec4F32Buffer { data: array<vec4<f32>> };");
    w.line("");

    // Match `runtime/indirect_args.rs` (`DrawIndexedIndirectArgs`).
    w.line("struct DrawIndexedIndirectArgs {");
    w.indent();
    w.line("index_count: u32,");
    w.line("instance_count: u32,");
    w.line("first_index: u32,");
    w.line("base_vertex: i32,");
    w.line("first_instance: u32,");
    w.dedent();
    w.line("};");
    w.line("");

    w.line("struct GsPrepassCounters {");
    w.indent();
    w.line("vertex_count: atomic<u32>,");
    w.line("index_count: atomic<u32>,");
    w.line("// Set to non-zero when any invocation detects OOB/overflow.");
    w.line("overflow: atomic<u32>,");
    w.line("_pad0: u32,");
    w.dedent();
    w.line("};");
    w.line("");

    // Uniform parameters are padded to 16 bytes (WebGPU uniform layout rules).
    w.line("struct GsPrepassParams {");
    w.indent();
    w.line("primitive_count: u32,");
    w.line("instance_count: u32,");
    w.line("first_instance: u32,");
    w.line("_pad0: u32,");
    w.dedent();
    w.line("};");
    w.line("");

    w.line(&format!(
        "const GS_INPUT_VERTS_PER_PRIM: u32 = {verts_per_primitive}u;"
    ));
    w.line(&format!(
        "const GS_INPUT_REG_COUNT: u32 = {gs_input_reg_count}u;"
    ));
    w.line(&format!(
        "const GS_MAX_VERTEX_COUNT: u32 = {max_output_vertices}u;"
    ));
    w.line(&format!(
        "const GS_INSTANCE_COUNT: u32 = {gs_instance_count}u;"
    ));
    w.line("");

    w.line("@group(0) @binding(0) var<storage, read_write> out_vertices: ExpandedVertexBuffer;");
    w.line("@group(0) @binding(1) var<storage, read_write> out_indices: U32Buffer;");
    w.line("@group(0) @binding(2) var<storage, read_write> out_indirect: DrawIndexedIndirectArgs;");
    w.line("@group(0) @binding(3) var<storage, read_write> counters: GsPrepassCounters;");
    w.line("@group(0) @binding(4) var<uniform> params: GsPrepassParams;");
    w.line("@group(0) @binding(5) var<storage, read> gs_inputs: Vec4F32Buffer;");
    w.line("");

    // GS input helper (v#[]).
    w.line("fn gs_load_input(prim_id: u32, reg: u32, vertex: u32) -> vec4<f32> {");
    w.indent();
    w.line("// Flattened index: ((prim_id * verts_per_prim + vertex) * reg_count + reg).");
    w.line("let idx = ((prim_id * GS_INPUT_VERTS_PER_PRIM + vertex) * GS_INPUT_REG_COUNT + reg);");
    w.line("let len = arrayLength(&gs_inputs.data);");
    w.line("if (idx >= len) {");
    w.indent();
    w.line("return vec4<f32>(0.0);");
    w.dedent();
    w.line("}");
    w.line("return gs_inputs.data[idx];");
    w.dedent();
    w.line("}");
    w.line("");

    // Cut semantics: restart strip assembly.
    w.line("fn gs_cut(strip_len: ptr<function, u32>) {");
    w.indent();
    w.line("*strip_len = 0u;");
    w.dedent();
    w.line("}");
    w.line("");

    // Emit semantics: append a vertex (built from o0/o1) and produce list indices based on the
    // GS output topology (point list, line strip, triangle strip).
    w.line("fn gs_emit(");
    w.indent();
    w.line("o0: vec4<f32>,");
    w.line("o1: vec4<f32>,");
    w.line("emitted_count: ptr<function, u32>,");
    w.line("strip_len: ptr<function, u32>,");
    w.line("strip_prev0: ptr<function, u32>,");
    w.line("strip_prev1: ptr<function, u32>,");
    w.line("overflow: ptr<function, bool>,");
    w.dedent();
    w.line(") {");
    w.indent();
    w.line("if (*overflow) { return; }");
    w.line("if (atomicLoad(&counters.overflow) != 0u) { *overflow = true; return; }");
    w.line("if (*emitted_count >= GS_MAX_VERTEX_COUNT) { return; }");
    w.line("");
    w.line("let vtx_idx = atomicAdd(&counters.vertex_count, 1u);");
    w.line("let vtx_cap = arrayLength(&out_vertices.data);");
    w.line("if (vtx_idx >= vtx_cap) {");
    w.indent();
    w.line("atomicOr(&counters.overflow, 1u);");
    w.line("*overflow = true;");
    w.line("return;");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("out_vertices.data[vtx_idx].pos = o0;");
    w.line("out_vertices.data[vtx_idx].o1 = o1;");
    w.line("");
    match output_topology_kind {
        OutputTopologyKind::PointList => {
            w.line("// Point list index emission.");
            w.line("let base = atomicAdd(&counters.index_count, 1u);");
            w.line("let idx_cap = arrayLength(&out_indices.data);");
            w.line("if (base >= idx_cap) {");
            w.indent();
            w.line("atomicOr(&counters.overflow, 1u);");
            w.line("*overflow = true;");
            w.line("return;");
            w.dedent();
            w.line("}");
            w.line("out_indices.data[base] = vtx_idx;");
        }
        OutputTopologyKind::LineStrip => {
            w.line("// Line strip -> line list index emission.");
            w.line("if (*strip_len == 0u) {");
            w.indent();
            w.line("*strip_prev0 = vtx_idx;");
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line("let base = atomicAdd(&counters.index_count, 2u);");
            w.line("let idx_cap = arrayLength(&out_indices.data);");
            w.line("if (base + 1u >= idx_cap) {");
            w.indent();
            w.line("atomicOr(&counters.overflow, 1u);");
            w.line("*overflow = true;");
            w.line("return;");
            w.dedent();
            w.line("}");
            w.line("out_indices.data[base] = *strip_prev0;");
            w.line("out_indices.data[base + 1u] = vtx_idx;");
            w.line("");
            w.line("// Advance strip assembly window.");
            w.line("*strip_prev0 = vtx_idx;");
            w.dedent();
            w.line("}");
        }
        OutputTopologyKind::TriangleStrip => {
            w.line("// Triangle strip -> triangle list index emission.");
            w.line("if (*strip_len == 0u) {");
            w.indent();
            w.line("*strip_prev0 = vtx_idx;");
            w.dedent();
            w.line("} else if (*strip_len == 1u) {");
            w.indent();
            w.line("*strip_prev1 = vtx_idx;");
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line("let i = *strip_len;");
            w.line("var a: u32;");
            w.line("var b: u32;");
            w.line("if ((i & 1u) == 0u) {");
            w.indent();
            w.line("a = *strip_prev0;");
            w.line("b = *strip_prev1;");
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line("a = *strip_prev1;");
            w.line("b = *strip_prev0;");
            w.dedent();
            w.line("}");
            w.line("");
            w.line("let base = atomicAdd(&counters.index_count, 3u);");
            w.line("let idx_cap = arrayLength(&out_indices.data);");
            w.line("if (base + 2u >= idx_cap) {");
            w.indent();
            w.line("atomicOr(&counters.overflow, 1u);");
            w.line("*overflow = true;");
            w.line("return;");
            w.dedent();
            w.line("}");
            w.line("out_indices.data[base] = a;");
            w.line("out_indices.data[base + 1u] = b;");
            w.line("out_indices.data[base + 2u] = vtx_idx;");
            w.line("");
            w.line("// Advance strip assembly window.");
            w.line("*strip_prev0 = *strip_prev1;");
            w.line("*strip_prev1 = vtx_idx;");
            w.dedent();
            w.line("}");
        }
    }
    w.line("*strip_len = *strip_len + 1u;");
    w.line("*emitted_count = *emitted_count + 1u;");
    w.dedent();
    w.line("}");
    w.line("");

    // Primitive entry point.
    w.line("fn gs_exec_primitive(");
    w.indent();
    w.line("prim_id: u32,");
    w.line("gs_instance_id_in: u32,");
    w.line("overflow: ptr<function, bool>,");
    w.dedent();
    w.line(") {");
    w.indent();
    w.line("if (*overflow) { return; }");
    w.line("if (atomicLoad(&counters.overflow) != 0u) { *overflow = true; return; }");
    w.line("");

    for i in 0..temp_reg_count {
        w.line(&format!("var r{i}: vec4<f32> = vec4<f32>(0.0);"));
    }
    for i in 0..output_reg_count {
        w.line(&format!("var o{i}: vec4<f32> = vec4<f32>(0.0);"));
    }
    w.line("");
    w.line("var emitted_count: u32 = 0u;");
    w.line("var strip_len: u32 = 0u;");
    w.line("var strip_prev0: u32 = 0u;");
    w.line("var strip_prev1: u32 = 0u;");
    w.line("");
    w.line("// Synthetic system values for compute-based GS emulation.");
    w.line("let primitive_id: u32 = prim_id;"); // SV_PrimitiveID
    w.line("let gs_instance_id: u32 = gs_instance_id_in;"); // SV_GSInstanceID
    w.line("");

    #[derive(Debug, Clone, Copy)]
    enum BlockKind {
        If { has_else: bool },
        Loop,
    }

    impl BlockKind {
        fn describe(&self) -> String {
            match self {
                BlockKind::If { has_else: false } => "if".to_owned(),
                BlockKind::If { has_else: true } => "if (else already seen)".to_owned(),
                BlockKind::Loop => "loop".to_owned(),
            }
        }

        fn expected_end_token(&self) -> &'static str {
            match self {
                BlockKind::If { .. } => "EndIf",
                BlockKind::Loop => "EndLoop",
            }
        }
    }

    let mut blocks: Vec<BlockKind> = Vec::new();

    #[derive(Debug, Clone, Copy)]
    enum SwitchLabel {
        Case(i32),
        Default,
    }

    #[derive(Debug, Default)]
    struct SwitchFrame {
        pending_labels: Vec<SwitchLabel>,
        saw_default: bool,
    }

    #[derive(Debug)]
    enum CfFrame {
        Switch(SwitchFrame),
        Case,
    }

    let mut cf_stack: Vec<CfFrame> = Vec::new();

    let fmt_case_values = |values: &[i32]| -> String {
        values
            .iter()
            .map(|v| format!("{v}i"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let close_case_body =
        |w: &mut WgslWriter, cf_stack: &mut Vec<CfFrame>| -> Result<(), GsTranslateError> {
            let Some(CfFrame::Case) = cf_stack.last() else {
                return Ok(());
            };

            // Close the WGSL case block.
            w.dedent();
            w.line("}");
            cf_stack.pop();
            Ok(())
        };

    let flush_pending_labels = |w: &mut WgslWriter,
                                cf_stack: &mut Vec<CfFrame>,
                                inst_index: usize|
     -> Result<(), GsTranslateError> {
        let pending_labels = match cf_stack.last_mut() {
            Some(CfFrame::Switch(sw)) => {
                if sw.pending_labels.is_empty() {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "case/default label inside switch".to_owned(),
                        found: "switch body without case label".to_owned(),
                    });
                }
                std::mem::take(&mut sw.pending_labels)
            }
            _ => return Ok(()),
        };

        let mut case_values = Vec::<i32>::new();
        let mut has_default = false;
        for lbl in &pending_labels {
            match *lbl {
                SwitchLabel::Case(v) => case_values.push(v),
                SwitchLabel::Default => has_default = true,
            }
        }

        let last_label = *pending_labels.last().expect("pending_labels non-empty");

        // If the label set contains a default label, we may need an extra empty clause stub, since
        // WGSL can't combine `default` with `case` selectors in a single clause.
        match (has_default, last_label) {
            (false, _) => {
                let selectors = fmt_case_values(&case_values);
                w.line(&format!("case {selectors}: {{"));
                w.indent();
                cf_stack.push(CfFrame::Case);
            }
            (true, SwitchLabel::Default) => {
                if !case_values.is_empty() {
                    let selectors = fmt_case_values(&case_values);
                    w.line(&format!("case {selectors}: {{"));
                    w.indent();
                    w.dedent();
                    w.line("}");
                }
                w.line("default: {");
                w.indent();
                cf_stack.push(CfFrame::Case);
            }
            (true, SwitchLabel::Case(_)) => {
                // Emit the default empty clause first so it can reach the case body.
                w.line("default: {");
                w.indent();
                w.dedent();
                w.line("}");
                let selectors = fmt_case_values(&case_values);
                w.line(&format!("case {selectors}: {{"));
                w.indent();
                cf_stack.push(CfFrame::Case);
            }
        }

        Ok(())
    };

    let emit_cmp = |inst_index: usize,
                    opcode: &'static str,
                    op: Sm4CmpOp,
                    a: &crate::sm4_ir::SrcOperand,
                    b: &crate::sm4_ir::SrcOperand|
     -> Result<String, GsTranslateError> {
        // Mirror the signature-driven shader translator: compare-based flow control performs
        // floating-point comparisons by default, with the `*_u` variants comparing the raw integer
        // bits as `u32`.
        let unsigned = matches!(
            op,
            Sm4CmpOp::EqU | Sm4CmpOp::NeU | Sm4CmpOp::LtU | Sm4CmpOp::GeU | Sm4CmpOp::LeU
                | Sm4CmpOp::GtU
        );
        let (a, b) = if unsigned {
            let a_u = emit_src_vec4_u32(inst_index, opcode, a, &input_sivs)?;
            let b_u = emit_src_vec4_u32(inst_index, opcode, b, &input_sivs)?;
            (format!("({a_u}).x"), format!("({b_u}).x"))
        } else {
            let a = emit_src_vec4(inst_index, opcode, a, &input_sivs)?;
            let b = emit_src_vec4(inst_index, opcode, b, &input_sivs)?;
            (format!("({a}).x"), format!("({b}).x"))
        };
        let op_str = match op {
            Sm4CmpOp::Eq | Sm4CmpOp::EqU => "==",
            Sm4CmpOp::Ne | Sm4CmpOp::NeU => "!=",
            Sm4CmpOp::Lt | Sm4CmpOp::LtU => "<",
            Sm4CmpOp::Le | Sm4CmpOp::LeU => "<=",
            Sm4CmpOp::Gt | Sm4CmpOp::GtU => ">",
            Sm4CmpOp::Ge | Sm4CmpOp::GeU => ">=",
        };
        Ok(format!("{a} {op_str} {b}"))
    };

    for (inst_index, inst) in module.instructions.iter().enumerate() {
        match inst {
            Sm4Inst::Case { value } => {
                close_case_body(&mut w, &mut cf_stack)?;

                let Some(CfFrame::Switch(sw)) = cf_stack.last_mut() else {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "switch".to_owned(),
                        found: "case".to_owned(),
                    });
                };
                sw.pending_labels.push(SwitchLabel::Case(*value as i32));
                continue;
            }
            Sm4Inst::Default => {
                close_case_body(&mut w, &mut cf_stack)?;

                let Some(CfFrame::Switch(sw)) = cf_stack.last_mut() else {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "switch".to_owned(),
                        found: "default".to_owned(),
                    });
                };
                sw.saw_default = true;
                sw.pending_labels.push(SwitchLabel::Default);
                continue;
            }
            Sm4Inst::EndSwitch => {
                // Close any open case body. If the clause falls through naturally (no `break;`),
                // reaching the end of the final clause still exits the `switch`.
                close_case_body(&mut w, &mut cf_stack)?;

                let Some(CfFrame::Switch(_)) = cf_stack.last() else {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "switch".to_owned(),
                        found: "endswitch".to_owned(),
                    });
                };

                let (pending_labels_nonempty, saw_default) = match cf_stack.last() {
                    Some(CfFrame::Switch(sw)) => (!sw.pending_labels.is_empty(), sw.saw_default),
                    _ => unreachable!("checked switch exists above"),
                };

                // If there are pending labels but no body, emit an empty clause.
                if pending_labels_nonempty {
                    flush_pending_labels(&mut w, &mut cf_stack, inst_index)?;
                    close_case_body(&mut w, &mut cf_stack)?;
                }

                // WGSL `switch` allows omitting `default`, but we always emit one so that
                // switch-without-default shaders stay structurally valid and match the HLSL
                // semantics where a missing default is equivalent to an empty one.
                if !saw_default {
                    w.line("default: {");
                    w.indent();
                    w.dedent();
                    w.line("}");
                }

                // Close the switch.
                w.dedent();
                w.line("}");
                cf_stack.pop();
                continue;
            }
            _ => {}
        }

        // Ensure any pending case labels are emitted before the first instruction of the clause
        // body.
        if matches!(cf_stack.last(), Some(CfFrame::Switch(_))) {
            flush_pending_labels(&mut w, &mut cf_stack, inst_index)?;
        }

        match inst {
            // ---- Structured control flow ----
            Sm4Inst::If { cond, test } => {
                let cond_vec = emit_src_vec4(inst_index, "if", cond, &input_sivs)?;
                let cond_scalar = format!("({cond_vec}).x");
                // DXBC register files are untyped 32-bit lanes. `if_z` / `if_nz` are defined as a
                // raw non-zero test on the underlying bits (not a float numeric compare).
                let cond_bits = format!("bitcast<u32>({cond_scalar})");
                let expr = match test {
                    Sm4TestBool::Zero => format!("{cond_bits} == 0u"),
                    Sm4TestBool::NonZero => format!("{cond_bits} != 0u"),
                };
                w.line(&format!("if ({expr}) {{"));
                w.indent();
                blocks.push(BlockKind::If { has_else: false });
            }
            Sm4Inst::IfC { op, a, b } => {
                let cond = emit_cmp(inst_index, "ifc", *op, a, b)?;
                w.line(&format!("if ({cond}) {{"));
                w.indent();
                blocks.push(BlockKind::If { has_else: false });
            }
            Sm4Inst::Else => {
                match blocks.last_mut() {
                    Some(BlockKind::If { has_else }) => {
                        if *has_else {
                            return Err(GsTranslateError::MalformedControlFlow {
                                inst_index,
                                expected: "if (without an else)".to_owned(),
                                found: BlockKind::If { has_else: true }.describe(),
                            });
                        }
                        *has_else = true;
                    }
                    Some(other) => {
                        return Err(GsTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "if".to_owned(),
                            found: other.describe(),
                        });
                    }
                    None => {
                        return Err(GsTranslateError::MalformedControlFlow {
                            inst_index,
                            expected: "if".to_owned(),
                            found: "none".to_owned(),
                        });
                    }
                }

                w.dedent();
                w.line("} else {");
                w.indent();
            }
            Sm4Inst::EndIf => match blocks.last() {
                Some(BlockKind::If { .. }) => {
                    blocks.pop();
                    w.dedent();
                    w.line("}");
                }
                Some(other) => {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "if".to_owned(),
                        found: other.describe(),
                    });
                }
                None => {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "if".to_owned(),
                        found: "none".to_owned(),
                    });
                }
            },
            Sm4Inst::Loop => {
                w.line("loop {");
                w.indent();
                blocks.push(BlockKind::Loop);
            }
            Sm4Inst::EndLoop => match blocks.last() {
                Some(BlockKind::Loop) => {
                    blocks.pop();
                    w.dedent();
                    w.line("}");
                }
                Some(other) => {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: other.describe(),
                    });
                }
                None => {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: "none".to_owned(),
                    });
                }
            },
            Sm4Inst::BreakC { op, a, b } => {
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_loop {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                let cond = emit_cmp(inst_index, "breakc", *op, a, b)?;
                w.line(&format!("if ({cond}) {{"));
                w.indent();
                w.line("break;");
                w.dedent();
                w.line("}");
            }
            Sm4Inst::ContinueC { op, a, b } => {
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_loop {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                let cond = emit_cmp(inst_index, "continuec", *op, a, b)?;
                w.line(&format!("if ({cond}) {{"));
                w.indent();
                w.line("continue;");
                w.dedent();
                w.line("}");
            }
            Sm4Inst::Switch { selector } => {
                // Integer instructions consume raw integer bits from the untyped register file.
                // Do not attempt to reinterpret float-typed sources as numeric integers.
                let selector_i = emit_src_vec4_i32(inst_index, "switch", selector, &input_sivs)?;
                let selector = format!("({selector_i}).x");
                w.line(&format!("switch({selector}) {{"));
                w.indent();
                cf_stack.push(CfFrame::Switch(SwitchFrame::default()));
            }
            Sm4Inst::Break => {
                let inside_case = matches!(cf_stack.last(), Some(CfFrame::Case));
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_case && !inside_loop {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop or switch case".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                w.line("break;");
            }
            Sm4Inst::Continue => {
                let inside_loop = blocks.iter().any(|b| matches!(b, BlockKind::Loop));
                if !inside_loop {
                    return Err(GsTranslateError::MalformedControlFlow {
                        inst_index,
                        expected: "loop".to_owned(),
                        found: blocks
                            .last()
                            .map(|b| b.describe())
                            .unwrap_or_else(|| "none".to_owned()),
                    });
                }
                w.line("continue;");
            }
            Sm4Inst::Mov { dst, src } => {
                let rhs = emit_src_vec4(inst_index, "mov", src, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, rhs);
                emit_write_masked(&mut w, inst_index, "mov", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Movc { dst, cond, a, b } => {
                let cond_vec = emit_src_vec4(inst_index, "movc", cond, &input_sivs)?;
                let a_vec = emit_src_vec4(inst_index, "movc", a, &input_sivs)?;
                let b_vec = emit_src_vec4(inst_index, "movc", b, &input_sivs)?;

                let cond_bits = format!("movc_cond_bits_{inst_index}");
                let cond_bool = format!("movc_cond_bool_{inst_index}");
                w.line(&format!(
                    "let {cond_bits} = bitcast<vec4<u32>>({cond_vec});"
                ));
                w.line(&format!("let {cond_bool} = {cond_bits} != vec4<u32>(0u);"));

                let rhs = maybe_saturate(
                    dst.saturate,
                    format!("select(({b_vec}), ({a_vec}), {cond_bool})"),
                );
                emit_write_masked(&mut w, inst_index, "movc", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Itof { dst, src } => {
                let src_i = emit_src_vec4_i32(inst_index, "itof", src, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("vec4<f32>({src_i})"));
                emit_write_masked(&mut w, inst_index, "itof", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Utof { dst, src } => {
                let src_u = emit_src_vec4_u32(inst_index, "utof", src, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("vec4<f32>({src_u})"));
                emit_write_masked(&mut w, inst_index, "utof", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Ftoi { dst, src } => {
                let src_f = emit_src_vec4(inst_index, "ftoi", src, &input_sivs)?;
                let rhs = format!("bitcast<vec4<f32>>(vec4<i32>({src_f}))");
                emit_write_masked(&mut w, inst_index, "ftoi", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Ftou { dst, src } => {
                let src_f = emit_src_vec4(inst_index, "ftou", src, &input_sivs)?;
                let rhs = format!("bitcast<vec4<f32>>(vec4<u32>({src_f}))");
                emit_write_masked(&mut w, inst_index, "ftou", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::F32ToF16 { dst, src } => {
                let src_f = emit_src_vec4(inst_index, "f32tof16", src, &input_sivs)?;
                let src_f = maybe_saturate(dst.saturate, src_f);

                let pack_lane = |c: char| {
                    format!("(pack2x16float(vec2<f32>(({src_f}).{c}, 0.0)) & 0xffffu)")
                };
                let ux = pack_lane('x');
                let uy = pack_lane('y');
                let uz = pack_lane('z');
                let uw = pack_lane('w');

                let rhs_u = format!("vec4<u32>({ux}, {uy}, {uz}, {uw})");
                let rhs = format!("bitcast<vec4<f32>>({rhs_u})");
                emit_write_masked(&mut w, inst_index, "f32tof16", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::F16ToF32 { dst, src } => {
                // Preserve the raw half-float bit-pattern by ignoring operand modifiers.
                let mut src_bits = src.clone();
                src_bits.modifier = OperandModifier::None;
                let src_u = emit_src_vec4_u32(inst_index, "f16tof32", &src_bits, &input_sivs)?;

                let unpack_lane = |c: char| format!("unpack2x16float((({src_u}).{c} & 0xffffu)).x");
                let x = unpack_lane('x');
                let y = unpack_lane('y');
                let z = unpack_lane('z');
                let w_lane = unpack_lane('w');

                let rhs = maybe_saturate(dst.saturate, format!("vec4<f32>({x}, {y}, {z}, {w_lane})"));
                emit_write_masked(&mut w, inst_index, "f16tof32", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Rcp { dst, src } => {
                let src = emit_src_vec4(inst_index, "rcp", src, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("1.0 / ({src})"));
                emit_write_masked(&mut w, inst_index, "rcp", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Rsq { dst, src } => {
                let src = emit_src_vec4(inst_index, "rsq", src, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("inverseSqrt({src})"));
                emit_write_masked(&mut w, inst_index, "rsq", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Add { dst, a, b } => {
                let a = emit_src_vec4(inst_index, "add", a, &input_sivs)?;
                let b = emit_src_vec4(inst_index, "add", b, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("({a}) + ({b})"));
                emit_write_masked(&mut w, inst_index, "add", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::And { dst, a, b } => {
                let a = emit_src_vec4_u32(inst_index, "and", a, &input_sivs)?;
                let b = emit_src_vec4_u32(inst_index, "and", b, &input_sivs)?;
                let rhs = format!("bitcast<vec4<f32>>(({a}) & ({b}))");
                emit_write_masked(&mut w, inst_index, "and", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Mul { dst, a, b } => {
                let a = emit_src_vec4(inst_index, "mul", a, &input_sivs)?;
                let b = emit_src_vec4(inst_index, "mul", b, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("({a}) * ({b})"));
                emit_write_masked(&mut w, inst_index, "mul", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Mad { dst, a, b, c } => {
                let a = emit_src_vec4(inst_index, "mad", a, &input_sivs)?;
                let b = emit_src_vec4(inst_index, "mad", b, &input_sivs)?;
                let c = emit_src_vec4(inst_index, "mad", c, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("({a}) * ({b}) + ({c})"));
                emit_write_masked(&mut w, inst_index, "mad", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Dp3 { dst, a, b } => {
                let a = emit_src_vec4(inst_index, "dp3", a, &input_sivs)?;
                let b = emit_src_vec4(inst_index, "dp3", b, &input_sivs)?;
                let rhs = format!("vec4<f32>(dot(({a}).xyz, ({b}).xyz))");
                let rhs = maybe_saturate(dst.saturate, rhs);
                emit_write_masked(&mut w, inst_index, "dp3", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Dp4 { dst, a, b } => {
                let a = emit_src_vec4(inst_index, "dp4", a, &input_sivs)?;
                let b = emit_src_vec4(inst_index, "dp4", b, &input_sivs)?;
                let rhs = format!("vec4<f32>(dot(({a}), ({b})))");
                let rhs = maybe_saturate(dst.saturate, rhs);
                emit_write_masked(&mut w, inst_index, "dp4", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Min { dst, a, b } => {
                let a = emit_src_vec4(inst_index, "min", a, &input_sivs)?;
                let b = emit_src_vec4(inst_index, "min", b, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("min(({a}), ({b}))"));
                emit_write_masked(&mut w, inst_index, "min", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Max { dst, a, b } => {
                let a = emit_src_vec4(inst_index, "max", a, &input_sivs)?;
                let b = emit_src_vec4(inst_index, "max", b, &input_sivs)?;
                let rhs = maybe_saturate(dst.saturate, format!("max(({a}), ({b}))"));
                emit_write_masked(&mut w, inst_index, "max", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Emit { stream: _ } => {
                w.line("gs_emit(o0, o1, &emitted_count, &strip_len, &strip_prev0, &strip_prev1, overflow); // emit");
            }
            Sm4Inst::Cut { stream: _ } => {
                w.line("gs_cut(&strip_len); // cut");
            }
            Sm4Inst::EmitThenCut { stream: _ } => {
                w.line(
                    "gs_emit(o0, o1, &emitted_count, &strip_len, &strip_prev0, &strip_prev1, overflow); // emitthen_cut",
                );
                w.line("gs_cut(&strip_len); // emitthen_cut");
            }
            Sm4Inst::Ret => {
                w.line("return;");
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {
                unreachable!("switch label instructions handled at top of loop")
            }
            other => {
                return Err(GsTranslateError::UnsupportedInstruction {
                    inst_index,
                    opcode: opcode_name(other),
                });
            }
        }
    }

    if let Some(open) = blocks.last() {
        return Err(GsTranslateError::MalformedControlFlow {
            inst_index: module.instructions.len(),
            expected: open.expected_end_token().to_owned(),
            found: open.describe(),
        });
    }
    if !cf_stack.is_empty() {
        return Err(GsTranslateError::MalformedControlFlow {
            inst_index: module.instructions.len(),
            expected: "EndSwitch".to_owned(),
            found: "end of shader".to_owned(),
        });
    }

    w.dedent();
    w.line("}");
    w.line("");

    // Compute entry point: one invocation per input primitive.
    w.line("@compute @workgroup_size(1)");
    w.line(&format!(
        "fn {entry_point}(@builtin(global_invocation_id) id: vec3<u32>) {{"
    ));
    w.indent();
    w.line("let prim_id: u32 = id.x;");
    w.line("if (prim_id >= params.primitive_count) { return; }");
    w.line("");
    w.line("var overflow: bool = false;");
    w.line(
        "for (var gs_instance_id: u32 = 0u; gs_instance_id < GS_INSTANCE_COUNT; gs_instance_id = gs_instance_id + 1u) {",
    );
    w.indent();
    w.line("gs_exec_primitive(prim_id, gs_instance_id, &overflow);");
    w.line("if (overflow) { break; }");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.line("");

    // Finalize pass: writes indirect args exactly once after the main prepass has completed.
    //
    // The executor should dispatch this with `dispatch_workgroups(1, 1, 1)` after running `cs_main`
    // for all primitives.
    w.line("@compute @workgroup_size(1)");
    w.line("fn cs_finalize(@builtin(global_invocation_id) id: vec3<u32>) {");
    w.indent();
    w.line("if (id.x != 0u) { return; }");
    w.line("let overflow: bool = atomicLoad(&counters.overflow) != 0u;");
    w.line("if (overflow) {");
    w.indent();
    w.line("out_indirect.index_count = 0u;");
    w.line("out_indirect.instance_count = 0u;");
    w.line("out_indirect.first_index = 0u;");
    w.line("out_indirect.base_vertex = 0;");
    w.line("out_indirect.first_instance = 0u;");
    w.dedent();
    w.line("} else {");
    w.indent();
    w.line("let out_index_count: u32 = atomicLoad(&counters.index_count);");
    w.line("out_indirect.index_count = out_index_count;");
    w.line("out_indirect.instance_count = params.instance_count;");
    w.line("out_indirect.first_index = 0u;");
    w.line("out_indirect.base_vertex = 0;");
    w.line("out_indirect.first_instance = params.first_instance;");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");

    Ok(GsPrepassTranslation {
        wgsl: w.finish(),
        info: GsPrepassInfo {
            input_verts_per_primitive: verts_per_primitive,
            input_reg_count: gs_input_reg_count,
            max_output_vertex_count: max_output_vertices,
        },
    })
}

// ---- WGSL emission helpers ----

fn maybe_saturate(saturate: bool, expr: String) -> String {
    if saturate {
        format!("clamp(({expr}), vec4<f32>(0.0), vec4<f32>(1.0))")
    } else {
        expr
    }
}

fn bump_reg_max(reg: RegisterRef, max_temp_reg: &mut i32, max_output_reg: &mut i32) {
    match reg.file {
        RegFile::Temp => *max_temp_reg = (*max_temp_reg).max(reg.index as i32),
        RegFile::Output => *max_output_reg = (*max_output_reg).max(reg.index as i32),
        RegFile::OutputDepth | RegFile::Input => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_src_operand(
    src: &crate::sm4_ir::SrcOperand,
    max_temp_reg: &mut i32,
    max_output_reg: &mut i32,
    max_gs_input_reg: &mut i32,
    verts_per_primitive: u32,
    inst_index: usize,
    opcode: &'static str,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<(), GsTranslateError> {
    match &src.kind {
        SrcKind::Register(reg) => {
            bump_reg_max(*reg, max_temp_reg, max_output_reg);
            match reg.file {
                RegFile::Temp | RegFile::Output => {}
                RegFile::Input => {
                    let info = input_sivs.get(&reg.index).ok_or_else(|| {
                        GsTranslateError::UnsupportedOperand {
                            inst_index,
                            opcode,
                            msg: format!(
                                "unsupported input register v{} (expected v#[]/SrcKind::GsInput or a supported system value via dcl_input_siv)",
                                reg.index
                            ),
                        }
                    })?;
                    match info.sys_value {
                        D3D_NAME_PRIMITIVE_ID | D3D_NAME_GS_INSTANCE_ID => {}
                        other => {
                            return Err(GsTranslateError::UnsupportedOperand {
                                inst_index,
                                opcode,
                                msg: format!(
                                    "unsupported input system value {other} for v{} (only SV_PrimitiveID/SV_GSInstanceID are supported)",
                                    reg.index
                                ),
                            })
                        }
                    }
                }
                other => {
                    // Keep this non-exhaustive: new `RegFile` variants should not break GS
                    // translation compilation; instead they should yield a descriptive runtime error.
                    let msg = {
                        // `RegFile` currently has a fixed set of variants, so the fallback arm is
                        // unreachable today. Keep it anyway so future variants degrade into a
                        // descriptive runtime error rather than a compiler error.
                        #[allow(unreachable_patterns)]
                        match other {
                            RegFile::OutputDepth => {
                                "RegFile::OutputDepth is not supported in GS prepass".to_owned()
                            }
                            _ => format!("unsupported source register file {other:?}"),
                        }
                    };
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg,
                    });
                }
            }
        }
        SrcKind::GsInput { reg, vertex } => {
            *max_gs_input_reg = (*max_gs_input_reg).max(*reg as i32);
            if *vertex >= verts_per_primitive {
                return Err(GsTranslateError::InvalidGsInputVertexIndex {
                    inst_index,
                    vertex: *vertex,
                    verts_per_primitive,
                });
            }
        }
        SrcKind::ImmediateF32(_) => {}
        other => {
            return Err(GsTranslateError::UnsupportedOperand {
                inst_index,
                opcode,
                msg: format!("unsupported source kind {other:?}"),
            })
        }
    }
    Ok(())
}

fn emit_write_masked(
    w: &mut WgslWriter,
    inst_index: usize,
    opcode: &'static str,
    dst: RegisterRef,
    mask: WriteMask,
    rhs: String,
) -> Result<(), GsTranslateError> {
    let dst_expr = match dst.file {
        RegFile::Temp => format!("r{}", dst.index),
        RegFile::Output => format!("o{}", dst.index),
        RegFile::OutputDepth => {
            return Err(GsTranslateError::UnsupportedOperand {
                inst_index,
                opcode,
                msg: "unsupported destination register file RegFile::OutputDepth".to_owned(),
            })
        }
        other => {
            return Err(GsTranslateError::UnsupportedOperand {
                inst_index,
                opcode,
                msg: format!("unsupported destination register file {other:?}"),
            })
        }
    };

    let mask_bits = mask.0 & 0xF;
    if mask_bits == 0 {
        return Ok(());
    }
    if mask_bits == 0xF {
        w.line(&format!("{dst_expr} = {rhs};"));
        return Ok(());
    }

    let comps = [('x', 1u8), ('y', 2u8), ('z', 4u8), ('w', 8u8)];
    for (c, bit) in comps {
        if (mask_bits & bit) != 0 {
            w.line(&format!("{dst_expr}.{c} = ({rhs}).{c};"));
        }
    }
    Ok(())
}

fn emit_src_vec4(
    inst_index: usize,
    opcode: &'static str,
    src: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<String, GsTranslateError> {
    let base = match &src.kind {
        SrcKind::Register(reg) => match reg.file {
            RegFile::Temp => format!("r{}", reg.index),
            RegFile::Output => format!("o{}", reg.index),
            RegFile::OutputDepth => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: "RegFile::OutputDepth is not supported in GS prepass".to_owned(),
                })
            }
            RegFile::Input => {
                let info = input_sivs.get(&reg.index).ok_or_else(|| {
                    GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: format!(
                            "unsupported input register v{} (expected v#[]/SrcKind::GsInput or a supported system value via dcl_input_siv)",
                            reg.index
                        ),
                    }
                })?;
                let u32_expr = match info.sys_value {
                    D3D_NAME_PRIMITIVE_ID => "primitive_id",
                    D3D_NAME_GS_INSTANCE_ID => "gs_instance_id",
                    other => {
                        return Err(GsTranslateError::UnsupportedOperand {
                            inst_index,
                            opcode,
                            msg: format!(
                                "unsupported input system value {other} for v{} (only SV_PrimitiveID/SV_GSInstanceID are supported)",
                                reg.index
                            ),
                        })
                    }
                };
                expand_u32_to_vec4(u32_expr, info.mask)
            }
        },
        SrcKind::GsInput { reg, vertex } => format!("gs_load_input(prim_id, {reg}u, {vertex}u)"),
        SrcKind::ImmediateF32(vals) => {
            let lanes: Vec<String> = vals
                .iter()
                .map(|v| format!("bitcast<f32>(0x{v:08x}u)"))
                .collect();
            format!(
                "vec4<f32>({}, {}, {}, {})",
                lanes[0], lanes[1], lanes[2], lanes[3]
            )
        }
        other => {
            return Err(GsTranslateError::UnsupportedOperand {
                inst_index,
                opcode,
                msg: format!("unsupported source kind {other:?}"),
            })
        }
    };

    let mut expr = base;
    if !src.swizzle.is_identity() {
        let s = swizzle_suffix(src.swizzle);
        expr = format!("({expr}).{s}");
    }
    expr = apply_modifier(expr, src.modifier);
    Ok(expr)
}

fn emit_src_vec4_u32(
    inst_index: usize,
    opcode: &'static str,
    src: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<String, GsTranslateError> {
    let base = match &src.kind {
        SrcKind::Register(reg) => match reg.file {
            RegFile::Temp => format!("bitcast<vec4<u32>>(r{})", reg.index),
            RegFile::Output => format!("bitcast<vec4<u32>>(o{})", reg.index),
            RegFile::Input => {
                let info = input_sivs.get(&reg.index).ok_or_else(|| {
                    GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: format!(
                            "unsupported input register v{} (expected v#[]/SrcKind::GsInput or a supported system value via dcl_input_siv)",
                            reg.index
                        ),
                    }
                })?;
                let u32_expr = match info.sys_value {
                    D3D_NAME_PRIMITIVE_ID => "primitive_id",
                    D3D_NAME_GS_INSTANCE_ID => "gs_instance_id",
                    other => {
                        return Err(GsTranslateError::UnsupportedOperand {
                            inst_index,
                            opcode,
                            msg: format!(
                                "unsupported input system value {other} for v{} (only SV_PrimitiveID/SV_GSInstanceID are supported)",
                                reg.index
                            ),
                        })
                    }
                };
                let f = expand_u32_to_vec4(u32_expr, info.mask);
                format!("bitcast<vec4<u32>>({f})")
            }
            RegFile::OutputDepth => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: "RegFile::OutputDepth is not supported in GS prepass".to_owned(),
                })
            }
        },
        SrcKind::GsInput { reg, vertex } => {
            format!("bitcast<vec4<u32>>(gs_load_input(prim_id, {reg}u, {vertex}u))")
        }
        SrcKind::ImmediateF32(vals) => {
            let lanes: Vec<String> = vals.iter().map(|v| format!("0x{v:08x}u")).collect();
            format!(
                "vec4<u32>({}, {}, {}, {})",
                lanes[0], lanes[1], lanes[2], lanes[3]
            )
        }
        other => {
            return Err(GsTranslateError::UnsupportedOperand {
                inst_index,
                opcode,
                msg: format!("unsupported source kind {other:?}"),
            })
        }
    };

    let mut expr = base;
    if !src.swizzle.is_identity() {
        let s = swizzle_suffix(src.swizzle);
        expr = format!("({expr}).{s}");
    }
    expr = apply_modifier_u32(expr, src.modifier);
    Ok(expr)
}

fn emit_src_vec4_i32(
    inst_index: usize,
    opcode: &'static str,
    src: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<String, GsTranslateError> {
    let base = match &src.kind {
        SrcKind::Register(reg) => match reg.file {
            RegFile::Temp => format!("bitcast<vec4<i32>>(r{})", reg.index),
            RegFile::Output => format!("bitcast<vec4<i32>>(o{})", reg.index),
            RegFile::Input => {
                let info = input_sivs.get(&reg.index).ok_or_else(|| {
                    GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: format!(
                            "unsupported input register v{} (expected v#[]/SrcKind::GsInput or a supported system value via dcl_input_siv)",
                            reg.index
                        ),
                    }
                })?;
                let u32_expr = match info.sys_value {
                    D3D_NAME_PRIMITIVE_ID => "primitive_id",
                    D3D_NAME_GS_INSTANCE_ID => "gs_instance_id",
                    other => {
                        return Err(GsTranslateError::UnsupportedOperand {
                            inst_index,
                            opcode,
                            msg: format!(
                                "unsupported input system value {other} for v{} (only SV_PrimitiveID/SV_GSInstanceID are supported)",
                                reg.index
                            ),
                        })
                    }
                };
                let f = expand_u32_to_vec4(u32_expr, info.mask);
                format!("bitcast<vec4<i32>>({f})")
            }
            RegFile::OutputDepth => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: "RegFile::OutputDepth is not supported in GS prepass".to_owned(),
                })
            }
        },
        SrcKind::GsInput { reg, vertex } => {
            format!("bitcast<vec4<i32>>(gs_load_input(prim_id, {reg}u, {vertex}u))")
        }
        SrcKind::ImmediateF32(vals) => {
            let lanes: Vec<String> = vals
                .iter()
                .map(|v| format!("bitcast<i32>(0x{v:08x}u)"))
                .collect();
            format!(
                "vec4<i32>({}, {}, {}, {})",
                lanes[0], lanes[1], lanes[2], lanes[3]
            )
        }
        other => {
            return Err(GsTranslateError::UnsupportedOperand {
                inst_index,
                opcode,
                msg: format!("unsupported source kind {other:?}"),
            })
        }
    };

    let mut expr = base;
    if !src.swizzle.is_identity() {
        let s = swizzle_suffix(src.swizzle);
        expr = format!("({expr}).{s}");
    }
    expr = apply_modifier(expr, src.modifier);
    Ok(expr)
}

fn expand_u32_to_vec4(expr_u32: &str, mask: WriteMask) -> String {
    // For system values, follow the same missing-component defaults as regular vertex inputs:
    // (0, 0, 0, 1).
    //
    // Note: system values like `SV_PrimitiveID` are integer-typed in D3D. We preserve raw integer
    // bits in our untyped `vec4<f32>` register model by bitcasting the u32 value into f32.
    //
    // For default fill, use the integer bit-patterns for 0/1. This ensures that applying `utof` to
    // the full vec4 (e.g. with identity swizzle) yields numeric 0/1 rather than large float values
    // caused by reinterpreting the float-typed `1.0` bit-pattern as an integer.
    let bits = mask.0 & 0xF;
    let lane = |bit: u8, default: &str| -> String {
        if (bits & bit) != 0 {
            format!("bitcast<f32>({expr_u32})")
        } else {
            default.to_owned()
        }
    };
    format!(
        "vec4<f32>({}, {}, {}, {})",
        lane(1, "0.0"),
        lane(2, "0.0"),
        lane(4, "0.0"),
        lane(8, "bitcast<f32>(1u)"),
    )
}

fn component_char(c: u8) -> char {
    match c {
        0 => 'x',
        1 => 'y',
        2 => 'z',
        3 => 'w',
        _ => 'x',
    }
}

fn swizzle_suffix(swizzle: Swizzle) -> String {
    let mut s = String::with_capacity(4);
    for &c in &swizzle.0 {
        s.push(component_char(c));
    }
    s
}

fn apply_modifier(expr: String, modifier: OperandModifier) -> String {
    match modifier {
        OperandModifier::None => expr,
        OperandModifier::Neg => format!("-({expr})"),
        OperandModifier::Abs => format!("abs({expr})"),
        OperandModifier::AbsNeg => format!("-abs({expr})"),
    }
}

fn apply_modifier_u32(expr: String, modifier: OperandModifier) -> String {
    match modifier {
        OperandModifier::None => expr,
        // WGSL does not support unary negation on `u32`. DXBC operand modifiers are defined over
        // raw 32-bit values, so model `-x` as wrapping subtraction from 0.
        OperandModifier::Neg | OperandModifier::AbsNeg => format!("vec4<u32>(0u) - ({expr})"),
        // `abs` is a no-op for unsigned integers.
        OperandModifier::Abs => expr,
    }
}

// ---- Simple WGSL writer ----

struct WgslWriter {
    out: String,
    indent: usize,
}

impl WgslWriter {
    fn new() -> Self {
        Self {
            out: String::new(),
            indent: 0,
        }
    }

    fn indent(&mut self) {
        self.indent += 2;
    }

    fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(2);
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push(' ');
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn finish(self) -> String {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sm4::ShaderModel;
    use crate::sm4_ir::{DstOperand, OperandModifier, RegisterRef, SrcKind, SrcOperand, Swizzle};

    #[test]
    fn gs_translate_supports_primitive_id_and_gs_instance_id_sivs() {
        let module = Sm4Module {
            stage: ShaderStage::Geometry,
            model: ShaderModel { major: 4, minor: 0 },
            decls: vec![
                Sm4Decl::GsInputPrimitive {
                    primitive: GsInputPrimitive::Point(1),
                },
                Sm4Decl::GsOutputTopology {
                    topology: GsOutputTopology::TriangleStrip(3),
                },
                Sm4Decl::GsMaxOutputVertexCount { max: 1 },
                Sm4Decl::InputSiv {
                    reg: 2,
                    mask: WriteMask::X,
                    sys_value: D3D_NAME_PRIMITIVE_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 3,
                    mask: WriteMask::X,
                    sys_value: D3D_NAME_GS_INSTANCE_ID,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::X,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 2,
                        }),
                        swizzle: Swizzle::XXXX,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Add {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 1,
                        },
                        mask: WriteMask::X,
                        saturate: false,
                    },
                    a: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        }),
                        swizzle: Swizzle::XXXX,
                        modifier: OperandModifier::None,
                    },
                    b: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 3,
                        }),
                        swizzle: Swizzle::XXXX,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");
        assert!(
            wgsl.contains("let primitive_id: u32 = prim_id;"),
            "expected primitive_id mapping in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("let gs_instance_id: u32 = gs_instance_id_in;"),
            "expected gs_instance_id mapping in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("bitcast<f32>(primitive_id)"),
            "expected primitive_id bitcast in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("bitcast<f32>(gs_instance_id)"),
            "expected gs_instance_id bitcast in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("bitcast<f32>(1u)"),
            "expected integer default-fill (w=1) to preserve raw bits in WGSL:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_emits_gs_instance_count_loop_when_declared() {
        let module = Sm4Module {
            stage: ShaderStage::Geometry,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::GsInputPrimitive {
                    primitive: GsInputPrimitive::Point(1),
                },
                Sm4Decl::GsOutputTopology {
                    topology: GsOutputTopology::TriangleStrip(3),
                },
                Sm4Decl::GsMaxOutputVertexCount { max: 1 },
                Sm4Decl::GsInstanceCount { count: 2 },
                Sm4Decl::InputSiv {
                    reg: 3,
                    mask: WriteMask::X,
                    sys_value: D3D_NAME_GS_INSTANCE_ID,
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::X,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Input,
                            index: 3,
                        }),
                        swizzle: Swizzle::XXXX,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");
        assert!(
            wgsl.contains("const GS_INSTANCE_COUNT: u32 = 2u;"),
            "expected GS_INSTANCE_COUNT constant in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("for (var gs_instance_id: u32 = 0u; gs_instance_id < GS_INSTANCE_COUNT;"),
            "expected per-instance loop in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("gs_exec_primitive(prim_id, gs_instance_id,"),
            "expected primitive invocation to receive gs_instance_id:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_emits_numeric_conversions() {
        // Ensure GS prepass translation supports explicit SM4 numeric conversion ops, which are
        // commonly emitted by HLSL when mixing integer and float math.
        let module = Sm4Module {
            stage: ShaderStage::Geometry,
            model: ShaderModel { major: 4, minor: 0 },
            decls: vec![
                Sm4Decl::GsInputPrimitive {
                    primitive: GsInputPrimitive::Point(1),
                },
                Sm4Decl::GsOutputTopology {
                    topology: GsOutputTopology::TriangleStrip(3),
                },
                Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            ],
            instructions: vec![
                // r0 = raw integer bits [1,2,3,4]
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([1, 2, 3, 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                // r1 = itof(r0)
                Sm4Inst::Itof {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                // r2 = ftoi(r1)
                Sm4Inst::Ftoi {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 2,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");
        assert!(
            wgsl.contains("vec4<f32>(bitcast<vec4<i32>>(r0))"),
            "expected itof to lower via bitcast<i32> then numeric cast:\n{wgsl}"
        );
        assert!(
            wgsl.contains("bitcast<vec4<f32>>(vec4<i32>(r1))"),
            "expected ftoi to lower via numeric cast to i32 then bitcast back to f32 bits:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_emits_half_float_conversions() {
        // Ensure GS prepass translation supports the SM4/SM5 half-float conversion ops
        // `f32tof16`/`f16tof32` using WGSL pack/unpack builtins (no `f16` types required).
        let module = Sm4Module {
            stage: ShaderStage::Geometry,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::GsInputPrimitive {
                    primitive: GsInputPrimitive::Point(1),
                },
                Sm4Decl::GsOutputTopology {
                    topology: GsOutputTopology::TriangleStrip(3),
                },
                Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            ],
            instructions: vec![
                // r0 = [1.0, 2.0, 3.0, 4.0]
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([
                            1.0f32.to_bits(),
                            2.0f32.to_bits(),
                            3.0f32.to_bits(),
                            4.0f32.to_bits(),
                        ]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                // r1 = f32tof16(r0) (half bits in low 16 bits of each lane)
                Sm4Inst::F32ToF16 {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                // r2 = f16tof32(r1)
                Sm4Inst::F16ToF32 {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 2,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Ret,
            ],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");
        assert!(
            wgsl.contains("pack2x16float"),
            "expected f32tof16 lowering to use pack2x16float:\n{wgsl}"
        );
        assert!(
            wgsl.contains("unpack2x16float"),
            "expected f16tof32 lowering to use unpack2x16float:\n{wgsl}"
        );
    }
}
