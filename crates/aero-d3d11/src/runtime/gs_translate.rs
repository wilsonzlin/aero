//! Geometry shader (GS) -> WGSL compute translation.
//!
//! WebGPU does not expose geometry shaders. For bring-up we emulate a small subset of SM4 geometry
//! shaders by translating the decoded [`crate::sm4_ir::Sm4Module`] into a WGSL compute shader that
//! performs a "geometry prepass":
//! - Executes the GS instruction stream per input primitive.
//! - Expands `emit`/`cut` output into a triangle-list index buffer (triangle strip assembly with
//!   restart semantics).
//! - Writes a `DrawIndexedIndirectArgs` struct so the render pass can consume the expanded
//!   geometry via `draw_indexed_indirect`.
//!
//! The initial implementation is intentionally minimal and only supports the instructions/operands
//! required by the Win7 GS tests (mov/add + immediate constants + `v#[]` inputs + `emit`/`cut` on
//! stream 0).

use core::fmt;

use crate::sm4::ShaderStage;
use crate::sm4_ir::{
    OperandModifier, RegFile, RegisterRef, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, Swizzle, WriteMask,
};

/// D3D10 tokenized program format: `D3D10_SB_PRIMITIVE`.
///
/// The input-primitive payload in `dcl_inputprimitive` uses a small numeric enum that matches the
/// D3D10 tokenized shader format. The project has historically seen FXC emit values that align
/// with the D3D primitive-topology constants (e.g. triangle=4), so the translator accepts multiple
/// common encodings to stay robust across toolchains.
const D3D10_SB_PRIMITIVE_POINT: u32 = 1;
const D3D10_SB_PRIMITIVE_LINE: u32 = 2;
const D3D10_SB_PRIMITIVE_TRIANGLE: u32 = 3;
const D3D10_SB_PRIMITIVE_LINE_ADJ: u32 = 6;
const D3D10_SB_PRIMITIVE_TRIANGLE_ADJ: u32 = 7;

/// D3D10 tokenized program format: `D3D10_SB_PRIMITIVE_TOPOLOGY`.
///
/// Values are sourced from the Windows SDK header `d3d10tokenizedprogramformat.h`.
// Note: `dcl_outputtopology` uses a small enum (point/line/triangle_strip).
// - Tokenized shader format encodes `triangle_strip` as 3.
// - Some toolchains/fixtures use D3D primitive-topology constants (`triangle_strip` = 5).
const D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP: u32 = 3;

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
        stream: u32,
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
        }
    }
}

impl std::error::Error for GsTranslateError {}

fn opcode_name(inst: &Sm4Inst) -> &'static str {
    match inst {
        Sm4Inst::If { .. } => "if",
        Sm4Inst::Else => "else",
        Sm4Inst::EndIf => "endif",
        Sm4Inst::Loop => "loop",
        Sm4Inst::EndLoop => "endloop",
        Sm4Inst::Mov { .. } => "mov",
        Sm4Inst::Movc { .. } => "movc",
        Sm4Inst::Utof { .. } => "utof",
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
        Sm4Inst::Emit { .. } => "emit",
        Sm4Inst::Cut { .. } => "cut",
        Sm4Inst::Ret => "ret",
        Sm4Inst::Unknown { .. } => "unknown",
        _ => "unimplemented",
    }
}

/// Translate a decoded SM4 geometry shader module into a WGSL compute shader implementing the
/// geometry prepass.
///
/// The generated WGSL uses the following fixed bind group layout:
/// - `@group(0) @binding(0)` expanded vertices buffer (`ExpandedVertexBuffer`, read_write)
/// - `@group(0) @binding(1)` expanded indices buffer (`U32Buffer`, read_write)
/// - `@group(0) @binding(2)` indirect args buffer (`DrawIndexedIndirectArgs`, read_write)
/// - `@group(0) @binding(3)` uniform params (`GsPrepassParams`)
/// - `@group(0) @binding(4)` GS input payload (`Vec4F32Buffer`, read)
pub fn translate_gs_module_to_wgsl_compute_prepass(
    module: &Sm4Module,
) -> Result<String, GsTranslateError> {
    if module.stage != ShaderStage::Geometry {
        return Err(GsTranslateError::NotGeometryStage(module.stage));
    }

    let mut input_primitive: Option<u32> = None;
    let mut output_topology: Option<u32> = None;
    let mut max_output_vertices: Option<u32> = None;
    for decl in &module.decls {
        match decl {
            Sm4Decl::GsInputPrimitive { primitive } => input_primitive = Some(*primitive),
            Sm4Decl::GsOutputTopology { topology } => output_topology = Some(*topology),
            Sm4Decl::GsMaxOutputVertexCount { max } => max_output_vertices = Some(*max),
            _ => {}
        }
    }

    let input_primitive =
        input_primitive.ok_or(GsTranslateError::MissingDecl("dcl_inputprimitive"))?;
    let output_topology =
        output_topology.ok_or(GsTranslateError::MissingDecl("dcl_outputtopology"))?;
    let max_output_vertices =
        max_output_vertices.ok_or(GsTranslateError::MissingDecl("dcl_maxvertexcount"))?;

    let verts_per_primitive = match input_primitive {
        D3D10_SB_PRIMITIVE_POINT => 1,
        D3D10_SB_PRIMITIVE_LINE => 2,
        // Triangle (accept both the tokenized-format value (3) and the D3D topology value
        // `D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST` (4) as seen in some fixtures).
        D3D10_SB_PRIMITIVE_TRIANGLE | 4 => 3,
        // Line adjacency (accept tokenized-format value (6) and D3D topology values
        // `LINELIST_ADJ` (10) / `LINESTRIP_ADJ` (11)).
        D3D10_SB_PRIMITIVE_LINE_ADJ | 10 | 11 => 4,
        // Triangle adjacency (accept tokenized-format value (7) and D3D topology values
        // `TRIANGLELIST_ADJ` (12) / `TRIANGLESTRIP_ADJ` (13)).
        D3D10_SB_PRIMITIVE_TRIANGLE_ADJ | 12 | 13 => 6,
        other => return Err(GsTranslateError::UnsupportedInputPrimitive { primitive: other }),
    };

    if output_topology != D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP && output_topology != 5 {
        return Err(GsTranslateError::UnsupportedOutputTopology {
            topology: output_topology,
        });
    }

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
                )?;
            }
            Sm4Inst::Add { dst, a, b } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    a,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "add",
                )?;
                scan_src_operand(
                    b,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "add",
                )?;
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
    w.line("");

    w.line("@group(0) @binding(0) var<storage, read_write> out_vertices: ExpandedVertexBuffer;");
    w.line("@group(0) @binding(1) var<storage, read_write> out_indices: U32Buffer;");
    w.line("@group(0) @binding(2) var<storage, read_write> out_indirect: DrawIndexedIndirectArgs;");
    w.line("@group(0) @binding(3) var<uniform> params: GsPrepassParams;");
    w.line("@group(0) @binding(4) var<storage, read> gs_inputs: Vec4F32Buffer;");
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

    // Emit semantics: append a vertex (built from o0/o1) and produce triangle-list indices from
    // triangle strip assembly state.
    w.line("fn gs_emit(");
    w.indent();
    w.line("o0: vec4<f32>,");
    w.line("o1: vec4<f32>,");
    w.line("out_vertex_count: ptr<function, u32>,");
    w.line("out_index_count: ptr<function, u32>,");
    w.line("emitted_count: ptr<function, u32>,");
    w.line("strip_len: ptr<function, u32>,");
    w.line("strip_prev0: ptr<function, u32>,");
    w.line("strip_prev1: ptr<function, u32>,");
    w.line("overflow: ptr<function, bool>,");
    w.dedent();
    w.line(") {");
    w.indent();
    w.line("if (*overflow) { return; }");
    w.line("if (*emitted_count >= GS_MAX_VERTEX_COUNT) { return; }");
    w.line("");
    w.line("let vtx_idx = *out_vertex_count;");
    w.line("let vtx_cap = arrayLength(&out_vertices.data);");
    w.line("if (vtx_idx >= vtx_cap) {");
    w.indent();
    w.line("*overflow = true;");
    w.line("return;");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("out_vertices.data[vtx_idx].pos = o0;");
    w.line("out_vertices.data[vtx_idx].o1 = o1;");
    w.line("*out_vertex_count = vtx_idx + 1u;");
    w.line("");
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
    w.line("let base = *out_index_count;");
    w.line("let idx_cap = arrayLength(&out_indices.data);");
    w.line("if (base + 2u >= idx_cap) {");
    w.indent();
    w.line("*overflow = true;");
    w.line("return;");
    w.dedent();
    w.line("}");
    w.line("out_indices.data[base] = a;");
    w.line("out_indices.data[base + 1u] = b;");
    w.line("out_indices.data[base + 2u] = vtx_idx;");
    w.line("*out_index_count = base + 3u;");
    w.line("");
    w.line("// Advance strip assembly window.");
    w.line("*strip_prev0 = *strip_prev1;");
    w.line("*strip_prev1 = vtx_idx;");
    w.dedent();
    w.line("}");
    w.line("*strip_len = *strip_len + 1u;");
    w.line("*emitted_count = *emitted_count + 1u;");
    w.dedent();
    w.line("}");
    w.line("");

    // Primitive entry point.
    w.line("fn gs_exec_primitive(");
    w.indent();
    w.line("prim_id: u32,");
    w.line("out_vertex_count: ptr<function, u32>,");
    w.line("out_index_count: ptr<function, u32>,");
    w.line("overflow: ptr<function, bool>,");
    w.dedent();
    w.line(") {");
    w.indent();
    w.line("if (*overflow) { return; }");
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

    for (inst_index, inst) in module.instructions.iter().enumerate() {
        match inst {
            Sm4Inst::Mov { dst, src } => {
                let rhs = emit_src_vec4(inst_index, "mov", src)?;
                let rhs = maybe_saturate(dst.saturate, rhs);
                emit_write_masked(&mut w, inst_index, "mov", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Add { dst, a, b } => {
                let a = emit_src_vec4(inst_index, "add", a)?;
                let b = emit_src_vec4(inst_index, "add", b)?;
                let rhs = maybe_saturate(dst.saturate, format!("({a}) + ({b})"));
                emit_write_masked(&mut w, inst_index, "add", dst.reg, dst.mask, rhs)?;
            }
            Sm4Inst::Emit { stream: _ } => {
                w.line(&format!(
                    "gs_emit(o0, o1, out_vertex_count, out_index_count, &emitted_count, &strip_len, &strip_prev0, &strip_prev1, overflow); // emit"
                ));
            }
            Sm4Inst::Cut { stream: _ } => {
                w.line("gs_cut(&strip_len); // cut");
            }
            Sm4Inst::Ret => {
                w.line("return;");
            }
            other => {
                return Err(GsTranslateError::UnsupportedInstruction {
                    inst_index,
                    opcode: opcode_name(other),
                });
            }
        }
    }

    w.dedent();
    w.line("}");
    w.line("");

    // Compute entry point.
    w.line("@compute @workgroup_size(1)");
    w.line("fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {");
    w.indent();
    w.line("if (id.x != 0u) { return; }");
    w.line("");
    w.line("var out_vertex_count: u32 = 0u;");
    w.line("var out_index_count: u32 = 0u;");
    w.line("var overflow: bool = false;");
    w.line("");
    w.line(
        "for (var prim_id: u32 = 0u; prim_id < params.primitive_count; prim_id = prim_id + 1u) {",
    );
    w.indent();
    w.line("gs_exec_primitive(prim_id, &out_vertex_count, &out_index_count, &overflow);");
    w.line("if (overflow) { break; }");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("if (overflow) {");
    w.indent();
    w.line("out_indirect.index_count = 0u;");
    w.dedent();
    w.line("} else {");
    w.indent();
    w.line("out_indirect.index_count = out_index_count;");
    w.dedent();
    w.line("}");
    w.line("out_indirect.instance_count = params.instance_count;");
    w.line("out_indirect.first_index = 0u;");
    w.line("out_indirect.base_vertex = 0;");
    w.line("out_indirect.first_instance = params.first_instance;");
    w.dedent();
    w.line("}");

    Ok(w.finish())
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
        _ => {}
    }
}

fn scan_src_operand(
    src: &crate::sm4_ir::SrcOperand,
    max_temp_reg: &mut i32,
    max_output_reg: &mut i32,
    max_gs_input_reg: &mut i32,
    verts_per_primitive: u32,
    inst_index: usize,
    opcode: &'static str,
) -> Result<(), GsTranslateError> {
    match &src.kind {
        SrcKind::Register(reg) => {
            bump_reg_max(*reg, max_temp_reg, max_output_reg);
            match reg.file {
                RegFile::Temp | RegFile::Output => {}
                RegFile::Input => {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: "RegFile::Input is not supported in GS prepass; expected v#[] (SrcKind::GsInput)".to_owned(),
                    });
                }
                RegFile::OutputDepth => {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: "RegFile::OutputDepth is not supported in GS prepass".to_owned(),
                    });
                }
                other => {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: format!("unsupported source register file {other:?}"),
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
) -> Result<String, GsTranslateError> {
    let base = match &src.kind {
        SrcKind::Register(reg) => match reg.file {
            RegFile::Temp => format!("r{}", reg.index),
            RegFile::Output => format!("o{}", reg.index),
            RegFile::Input => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: "RegFile::Input is not supported in GS prepass; expected v#[] (SrcKind::GsInput)"
                        .to_owned(),
                })
            }
            other => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: match other {
                        RegFile::OutputDepth => {
                            "RegFile::OutputDepth is not supported in GS prepass".to_owned()
                        }
                        other => format!("unsupported source register file {other:?}"),
                    },
                })
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
