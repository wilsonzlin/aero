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
//! The generated compute shader keeps its internal prepass resources in `@group(0)` and declares
//! any referenced D3D geometry-stage resources (constant buffers `b#` / `cb#[]`, SRVs `t#`,
//! samplers `s#`) in the shared executor bind group `@group(3)`.
//!
//! Note: this module is only **partially wired** into the AeroGPU command executor today:
//! - `CREATE_SHADER_DXBC` attempts to translate SM4/SM5 GS DXBC into a WGSL compute prepass.
//! - Point-list, line-list, and triangle-list `DRAW`/`DRAW_INDEXED` can execute the translated compute prepass
//!   when translation succeeds. The post-expansion render pass honors the GS declared output
//!   topology (including `pointlist`; strip outputs are expanded to list indices).
//!
//! Most GS/HS/DS draws still route through the built-in compute-prepass WGSL shaders
//! (`GEOMETRY_PREPASS_CS_WGSL` / `GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL`) used for bring-up and for
//! topologies/draw paths where translated GS DXBC execution is not implemented yet.
//!
//! Note: if a GS DXBC blob cannot be translated by this module, the shader handle is still created,
//! but draws with that GS bound currently return a clear error (there is not yet a “run synthetic
//! expansion anyway” fallback for arbitrary untranslatable GS bytecode).
//!
//! The initial implementation is intentionally minimal and focuses on a small SM4 subset required
//! by the in-tree GS tests:
//! - Primitive emission: `emit` / `cut` / `emitthen_cut` (stream 0 only)
//! - Predicate registers + predication: `setp` and DXBC instruction predication for non-control-flow
//!   instructions
//! - A small ALU subset (`mov`/`movc`/`add`/`mul`/`mad`/`dp3`/`dp4`/`min`/`max`, plus `rcp`/`rsq`,
//!   integer/bitwise ops (`iadd`/`isub`/`udiv`/`idiv`, `and`/`or`/`xor`/`not`, shifts, min/max/abs/neg,
//!   predicate-mask compares, and bitfield/bitcount helpers), and a handful of pack/unpack +
//!   int/float conversion ops)
//! - Structured control flow (`if`/`else`/`loop`/`switch` with `break`/`continue`)
//!
//! Resource support is also intentionally limited:
//! - Read-only Texture2D ops (`sample`, `sample_l`, `ld`, `resinfo`)
//! - Read-only SRV buffer ops (`ld_raw`, `ld_structured`, `bufinfo`)
//! - SM5 UAV buffer ops (`ld_uav_raw`, `ld_structured_uav`, `store_raw`, `store_structured`,
//!   `atomic_add`, `bufinfo` on `u#`)
//! - SM5 typed UAV texture stores (`store_uav_typed` with `dcl_uav_typed` for `RWTexture2D`)
//!
//! Other resource writes/stores (e.g. typed UAV loads, other texture/UAV dimensions), and
//! barrier/synchronization instructions are not supported, and any unsupported instruction or
//! operand shape is rejected by translation.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::binding_model::{
    BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, BINDING_BASE_UAV,
    BIND_GROUP_INTERNAL_EMULATION, EXPANDED_VERTEX_MAX_VARYINGS,
};
use crate::shader_translate::StorageTextureFormat;
use crate::sm4::ShaderStage;
use crate::sm4_ir::{
    BufferKind, CmpOp, CmpType, GsInputPrimitive, GsOutputTopology, OperandModifier,
    PredicateDstOperand, PredicateOperand, RegFile, RegisterRef, Sm4CmpOp, Sm4Decl, Sm4Inst,
    Sm4Module, Sm4TestBool, SrcKind, Swizzle, WriteMask,
};

/// `@group(0)` binding numbers used by the translated GS compute prepass.
///
/// The WGSL emitted by this module and the executor-side bind group layouts must agree on these
/// values; keeping them centralized avoids accidental divergence (which would surface as wgpu
/// pipeline layout validation errors).
pub const GS_PREPASS_BINDING_OUT_VERTICES: u32 = 0;
pub const GS_PREPASS_BINDING_OUT_INDICES: u32 = 1;
pub const GS_PREPASS_BINDING_OUT_STATE: u32 = 2;
pub const GS_PREPASS_BINDING_PARAMS: u32 = 4;
pub const GS_PREPASS_BINDING_GS_INPUTS: u32 = 5;

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
    InvalidVaryingLocation {
        /// Output register location (`o#`) requested as a varying.
        ///
        /// Location 0 is reserved for position in the expanded-vertex scheme.
        ///
        /// Valid varying locations are `1..EXPANDED_VERTEX_MAX_VARYINGS`.
        loc: u32,
    },
    OutputRegisterOutOfRange {
        /// Highest output register location (`o#`) referenced by the shader.
        loc: u32,
        /// Maximum supported location for the current expanded-vertex layout.
        max_supported: u32,
    },
    UnsupportedInputPrimitive {
        primitive: u32,
    },
    UnsupportedOutputTopology {
        topology: u32,
    },
    UnsupportedOutputRegister {
        reg: u32,
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
            GsTranslateError::InvalidVaryingLocation { loc } => write!(
                f,
                "GS translate: invalid output varying location {loc} (valid range is 1..{}; location 0 is reserved for position in the expanded-vertex scheme)",
                EXPANDED_VERTEX_MAX_VARYINGS.saturating_sub(1),
            ),
            GsTranslateError::OutputRegisterOutOfRange { loc, max_supported } => write!(
                f,
                "GS translate: output register o{loc} is out of range for the fixed expanded-vertex layout (max o{max_supported})"
            ),
            GsTranslateError::UnsupportedInputPrimitive { primitive } => write!(
                f,
                "GS translate: unsupported input primitive {primitive} (dcl_inputprimitive)"
            ),
            GsTranslateError::UnsupportedOutputTopology { topology } => write!(
                f,
                "GS translate: unsupported output topology {topology} (dcl_outputtopology)"
            ),
            GsTranslateError::UnsupportedOutputRegister { reg } => write!(
                f,
                "GS translate: unsupported output register o{reg} (expanded-vertex format supports o0..o{})",
                EXPANDED_VERTEX_MAX_VARYINGS.saturating_sub(1)
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
        Sm4Inst::Predicated { .. } => "predicated",
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
        Sm4Inst::Setp { .. } => "setp",
        Sm4Inst::Itof { .. } => "itof",
        Sm4Inst::Utof { .. } => "utof",
        Sm4Inst::Ftoi { .. } => "ftoi",
        Sm4Inst::Ftou { .. } => "ftou",
        Sm4Inst::F32ToF16 { .. } => "f32tof16",
        Sm4Inst::F16ToF32 { .. } => "f16tof32",
        Sm4Inst::And { .. } => "and",
        Sm4Inst::Add { .. } => "add",
        Sm4Inst::IAdd { .. } => "iadd",
        Sm4Inst::ISub { .. } => "isub",
        Sm4Inst::UMul { .. } => "umul",
        Sm4Inst::IMul { .. } => "imul",
        Sm4Inst::UMad { .. } => "umad",
        Sm4Inst::IMad { .. } => "imad",
        Sm4Inst::Or { .. } => "or",
        Sm4Inst::Xor { .. } => "xor",
        Sm4Inst::Not { .. } => "not",
        Sm4Inst::IShl { .. } => "ishl",
        Sm4Inst::IShr { .. } => "ishr",
        Sm4Inst::UShr { .. } => "ushr",
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
        Sm4Inst::FirstbitHi { .. } => "firstbit_hi",
        Sm4Inst::FirstbitLo { .. } => "firstbit_lo",
        Sm4Inst::FirstbitShi { .. } => "firstbit_shi",
        Sm4Inst::Sample { .. } => "sample",
        Sm4Inst::SampleL { .. } => "sample_l",
        Sm4Inst::ResInfo { .. } => "resinfo",
        Sm4Inst::Ld { .. } => "ld",
        Sm4Inst::LdRaw { .. } => "ld_raw",
        Sm4Inst::LdUavRaw { .. } => "ld_uav_raw",
        Sm4Inst::StoreRaw { .. } => "store_raw",
        Sm4Inst::LdStructured { .. } => "ld_structured",
        Sm4Inst::LdStructuredUav { .. } => "ld_structured_uav",
        Sm4Inst::StoreStructured { .. } => "store_structured",
        Sm4Inst::StoreUavTyped { .. } => "store_uav_typed",
        Sm4Inst::AtomicAdd { .. } => "atomic_add",
        Sm4Inst::BufInfoRaw { .. }
        | Sm4Inst::BufInfoStructured { .. }
        | Sm4Inst::BufInfoRawUav { .. }
        | Sm4Inst::BufInfoStructuredUav { .. } => "bufinfo",
        Sm4Inst::Sync { .. } => "sync",
        Sm4Inst::Emit { .. } => "emit",
        Sm4Inst::Cut { .. } => "cut",
        Sm4Inst::EmitThenCut { .. } => "emitthen_cut",
        Sm4Inst::Ret => "ret",
        Sm4Inst::Unknown { .. } => "unknown",
        _ => "unsupported",
    }
}

/// The GS declared output topology kind (`dcl_outputtopology`).
///
/// Note: the compute prepass lowers strip output (`line_strip` / `triangle_strip`) into list
/// indices (`line_list` / `triangle_list`). Use this kind to determine the post-expansion render
/// primitive topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GsOutputTopologyKind {
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
) -> Result<GsOutputTopologyKind, GsTranslateError> {
    let input_prim_token = input_primitive_token(input_primitive);
    let likely_d3d_encoding = matches!(input_prim_token, 4 | 10 | 11 | 12 | 13);

    match topology {
        GsOutputTopology::Point(_) => Ok(GsOutputTopologyKind::PointList),
        GsOutputTopology::LineStrip(_) => Ok(GsOutputTopologyKind::LineStrip),
        // `3` is ambiguous:
        // - tokenized shader format: trianglestrip=3.
        // - D3D primitive topology enum: linestrip=3.
        //
        // Prefer the tokenized interpretation by default, but accept the D3D encoding when the
        // input primitive encoding strongly suggests the toolchain is emitting D3D topology
        // constants for GS declarations (e.g. triangle=4, triadj=12).
        GsOutputTopology::TriangleStrip(3) => {
            if likely_d3d_encoding {
                Ok(GsOutputTopologyKind::LineStrip)
            } else {
                Ok(GsOutputTopologyKind::TriangleStrip)
            }
        }
        GsOutputTopology::TriangleStrip(_) => Ok(GsOutputTopologyKind::TriangleStrip),
        GsOutputTopology::Unknown(other) => match other {
            1 => Ok(GsOutputTopologyKind::PointList),
            2 => Ok(GsOutputTopologyKind::LineStrip),
            3 => {
                if likely_d3d_encoding {
                    Ok(GsOutputTopologyKind::LineStrip)
                } else {
                    Ok(GsOutputTopologyKind::TriangleStrip)
                }
            }
            5 => Ok(GsOutputTopologyKind::TriangleStrip),
            _ => Err(GsTranslateError::UnsupportedOutputTopology { topology: other }),
        },
    }
}

#[cfg(test)]
fn module_output_topology_kind(
    module: &Sm4Module,
) -> Result<GsOutputTopologyKind, GsTranslateError> {
    let mut input_primitive: Option<GsInputPrimitive> = None;
    let mut output_topology: Option<GsOutputTopology> = None;
    for decl in &module.decls {
        match decl {
            Sm4Decl::GsInputPrimitive { primitive } => input_primitive = Some(*primitive),
            Sm4Decl::GsOutputTopology { topology } => output_topology = Some(*topology),
            _ => {}
        }
    }

    let input_primitive =
        input_primitive.ok_or(GsTranslateError::MissingDecl("dcl_inputprimitive"))?;
    let output_topology =
        output_topology.ok_or(GsTranslateError::MissingDecl("dcl_outputtopology"))?;
    decode_output_topology(output_topology, input_primitive)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GsPrepassInfo {
    /// Number of vertices in each input primitive (point=1, line=2, triangle=3, etc).
    pub input_verts_per_primitive: u32,
    /// Number of input registers (`v#[]`) referenced by the shader (1 + max register index).
    pub input_reg_count: u32,
    /// Maximum number of vertices the shader may emit per input primitive (`dcl_maxvertexcount`).
    pub max_output_vertex_count: u32,
    /// Geometry shader declared output topology kind (`dcl_outputtopology`).
    ///
    /// Note: the compute prepass expands strips into lists, so:
    /// - `LineStrip` expands to a render `LineList`
    /// - `TriangleStrip` expands to a render `TriangleList`
    pub output_topology_kind: GsOutputTopologyKind,
}

#[derive(Debug, Clone)]
pub struct GsPrepassTranslation {
    pub wgsl: String,
    pub info: GsPrepassInfo,
}

fn default_varyings_from_decls(module: &Sm4Module) -> Vec<u32> {
    // The caller-facing translation helpers default to exporting all declared GS output registers
    // (excluding position at o0) into the expanded-vertex varying table.
    //
    // This keeps the behavior robust for typical FXC/DXC output signatures without requiring the
    // executor to know which pixel-shader varyings will be consumed at draw time.
    //
    // Some toolchains omit explicit `dcl_output` tokens even when the output signature (`OSGN`)
    // contains non-position varyings. To keep translation robust across fixtures, also include any
    // output registers written by the instruction stream.
    //
    // Note: System-value outputs (`Sm4Decl::OutputSiv`) are currently ignored by the GS prepass
    // translator; position is always assumed to be `o0` and is stored in `ExpandedVertex.pos`.

    let mut out: BTreeSet<u32> = BTreeSet::new();

    // Declared outputs (`dcl_output o#.*`).
    for decl in &module.decls {
        let Sm4Decl::Output { reg, mask } = decl else {
            continue;
        };
        if *reg == 0 || mask.0 == 0 {
            continue;
        }
        out.insert(*reg);
    }

    fn maybe_add_dst(dst: &crate::sm4_ir::DstOperand, out: &mut BTreeSet<u32>) {
        if dst.mask.0 == 0 {
            return;
        }
        if dst.reg.file != RegFile::Output {
            return;
        }
        if dst.reg.index == 0 {
            return;
        }
        out.insert(dst.reg.index);
    }

    fn scan_inst(inst: &Sm4Inst, out: &mut BTreeSet<u32>) {
        use Sm4Inst::*;

        match inst {
            Predicated { inner, .. } => scan_inst(inner, out),
            Mov { dst, .. }
            | Movc { dst, .. }
            | Utof { dst, .. }
            | Itof { dst, .. }
            | Ftoi { dst, .. }
            | Ftou { dst, .. }
            | And { dst, .. }
            | Add { dst, .. }
            | Mul { dst, .. }
            | Mad { dst, .. }
            | Dp3 { dst, .. }
            | Dp4 { dst, .. }
            | Min { dst, .. }
            | Max { dst, .. }
            | IAdd { dst, .. }
            | ISub { dst, .. }
            | Or { dst, .. }
            | Xor { dst, .. }
            | Not { dst, .. }
            | IShl { dst, .. }
            | IShr { dst, .. }
            | UShr { dst, .. }
            | Cmp { dst, .. }
            | IMin { dst, .. }
            | IMax { dst, .. }
            | UMin { dst, .. }
            | UMax { dst, .. }
            | IAbs { dst, .. }
            | INeg { dst, .. }
            | Rcp { dst, .. }
            | Rsq { dst, .. }
            | Bfi { dst, .. }
            | Ubfe { dst, .. }
            | Ibfe { dst, .. }
            | Bfrev { dst, .. }
            | CountBits { dst, .. }
            | FirstbitHi { dst, .. }
            | FirstbitLo { dst, .. }
            | FirstbitShi { dst, .. }
            | F32ToF16 { dst, .. }
            | F16ToF32 { dst, .. }
            | Sample { dst, .. }
            | SampleL { dst, .. }
            | ResInfo { dst, .. }
            | Ld { dst, .. }
            | LdRaw { dst, .. }
            | LdUavRaw { dst, .. }
            | LdStructured { dst, .. }
            | LdStructuredUav { dst, .. }
            | BufInfoRaw { dst, .. }
            | BufInfoStructured { dst, .. }
            | BufInfoRawUav { dst, .. }
            | BufInfoStructuredUav { dst, .. } => {
                maybe_add_dst(dst, out);
            }
            IMul { dst_lo, dst_hi, .. }
            | UMul { dst_lo, dst_hi, .. }
            | IMad { dst_lo, dst_hi, .. }
            | UMad { dst_lo, dst_hi, .. } => {
                maybe_add_dst(dst_lo, out);
                if let Some(dst_hi) = dst_hi.as_ref() {
                    maybe_add_dst(dst_hi, out);
                }
            }
            IAddC {
                dst_sum, dst_carry, ..
            }
            | UAddC {
                dst_sum, dst_carry, ..
            } => {
                maybe_add_dst(dst_sum, out);
                maybe_add_dst(dst_carry, out);
            }
            ISubC {
                dst_diff,
                dst_carry,
                ..
            } => {
                maybe_add_dst(dst_diff, out);
                maybe_add_dst(dst_carry, out);
            }
            USubB {
                dst_diff,
                dst_borrow,
                ..
            } => {
                maybe_add_dst(dst_diff, out);
                maybe_add_dst(dst_borrow, out);
            }
            UDiv {
                dst_quot, dst_rem, ..
            }
            | IDiv {
                dst_quot, dst_rem, ..
            } => {
                maybe_add_dst(dst_quot, out);
                maybe_add_dst(dst_rem, out);
            }
            AtomicAdd { dst, .. } => {
                if let Some(dst) = dst.as_ref() {
                    maybe_add_dst(dst, out);
                }
            }
            // Non-register-writing instructions.
            _ => {}
        }
    }

    for inst in &module.instructions {
        scan_inst(inst, &mut out);
    }

    out.into_iter().collect()
}

#[derive(Clone, Copy, Debug)]
enum ExpandedVertexOutputLayout<'a> {
    /// Pack only the requested output register locations into consecutive `vN` fields on
    /// `ExpandedVertex`.
    Packed { varyings: &'a [u32] },
    /// Use the legacy expanded-vertex layout expected by the current cmd-stream executor
    /// passthrough VS (`pos + varyings: array<vec4<f32>, EXPANDED_VERTEX_MAX_VARYINGS>`), writing
    /// output registers into the corresponding `varyings[loc]` slot.
    FixedArray { max_varyings: u32 },
}

/// Translate a decoded SM4 geometry shader module into a WGSL compute shader implementing the
/// geometry prepass, selecting which output varyings are written into the expanded vertex buffer.
///
/// `varyings` is a sorted, de-duplicated list of D3D output register locations (`o#`) that should be
/// packed into the expanded vertex buffer, in slice order.
///
/// `o0` / position is always written separately as the first field (`ExpandedVertex.pos`).
///
/// - Location 0 is reserved for position (`o0`) and must not be included in `varyings`.
/// - Locations must be in `1..EXPANDED_VERTEX_MAX_VARYINGS`.
///
/// The generated WGSL uses the following fixed bind group layout:
/// - `@group(0) @binding(0)` expanded vertices buffer (`ExpandedVertexBuffer`, read_write)
/// - `@group(0) @binding(1)` expanded indices buffer (`U32Buffer`, read_write)
/// - `@group(0) @binding(2)` state buffer (`GsPrepassState`, read_write)
///   - `state.out_indirect`: `DrawIndexedIndirectArgs` at offset 0 so the render pass can call
///     `draw_indexed_indirect` with the same buffer+offset.
///   - `state.counters`: `GsPrepassCounters`
/// - `@group(0) @binding(4)` uniform params (`GsPrepassParams`)
/// - `@group(0) @binding(5)` GS input payload (`Vec4F32Buffer`, read_write)
/// - `@group(3)` referenced `b#`/`t#`/`s#` resources following the shared executor binding model
///   (e.g. `@binding(BINDING_BASE_CBUFFER + slot)`).
pub fn translate_gs_module_to_wgsl_compute_prepass_packed(
    module: &Sm4Module,
    varyings: &[u32],
) -> Result<String, GsTranslateError> {
    Ok(
        translate_gs_module_to_wgsl_compute_prepass_with_entry_point_impl(
            module,
            "cs_main",
            ExpandedVertexOutputLayout::Packed { varyings },
        )?
        .wgsl,
    )
}

/// Translate a decoded SM4 geometry shader module into a WGSL compute shader implementing the
/// geometry prepass.
///
/// The generated WGSL uses the following fixed bind group layout:
/// - `@group(0) @binding(0)` expanded vertices buffer (`ExpandedVertexBuffer`, read_write)
/// - `@group(0) @binding(1)` expanded indices buffer (`U32Buffer`, read_write)
/// - `@group(0) @binding(2)` state buffer (`GsPrepassState`, read_write)
///   - `state.out_indirect`: `DrawIndexedIndirectArgs`
///   - `state.counters`: `GsPrepassCounters`
/// - `@group(0) @binding(4)` uniform params (`GsPrepassParams`)
/// - `@group(0) @binding(5)` GS input payload (`Vec4F32Buffer`, read_write)
/// - `@group(3)` referenced `b#`/`t#`/`s#` resources following the shared executor binding model
///   (e.g. `@binding(BINDING_BASE_CBUFFER + slot)`).
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
    let varyings = default_varyings_from_decls(module);
    translate_gs_module_to_wgsl_compute_prepass_with_entry_point_impl(
        module,
        entry_point,
        ExpandedVertexOutputLayout::Packed {
            varyings: varyings.as_slice(),
        },
    )
}

/// Internal executor helper: translate using the fixed expanded-vertex layout
/// (`ExpandedVertex { pos, varyings: array<vec4<f32>, EXPANDED_VERTEX_MAX_VARYINGS> }`).
pub(crate) fn translate_gs_module_to_wgsl_compute_prepass_with_entry_point_fixed(
    module: &Sm4Module,
    entry_point: &str,
) -> Result<GsPrepassTranslation, GsTranslateError> {
    translate_gs_module_to_wgsl_compute_prepass_with_entry_point_impl(
        module,
        entry_point,
        ExpandedVertexOutputLayout::FixedArray {
            max_varyings: EXPANDED_VERTEX_MAX_VARYINGS,
        },
    )
}

fn translate_gs_module_to_wgsl_compute_prepass_with_entry_point_impl(
    module: &Sm4Module,
    entry_point: &str,
    expanded_vertex_layout: ExpandedVertexOutputLayout<'_>,
) -> Result<GsPrepassTranslation, GsTranslateError> {
    if module.stage != ShaderStage::Geometry {
        return Err(GsTranslateError::NotGeometryStage(module.stage));
    }

    if let ExpandedVertexOutputLayout::Packed { varyings } = expanded_vertex_layout {
        if let Some(&loc) = varyings
            .iter()
            .find(|&&loc| loc == 0 || loc >= EXPANDED_VERTEX_MAX_VARYINGS)
        {
            return Err(GsTranslateError::InvalidVaryingLocation { loc });
        }
    }

    let mut input_primitive: Option<GsInputPrimitive> = None;
    let mut output_topology: Option<GsOutputTopology> = None;
    let mut max_output_vertices: Option<u32> = None;
    let mut gs_instance_count: Option<u32> = None;
    let mut input_sivs: HashMap<u32, InputSivInfo> = HashMap::new();
    let mut cbuffer_decls: BTreeMap<u32, u32> = BTreeMap::new();
    let mut texture2d_decls: BTreeSet<u32> = BTreeSet::new();
    let mut sampler_decls: BTreeSet<u32> = BTreeSet::new();
    let mut srv_buffer_decls: BTreeMap<u32, (BufferKind, u32)> = BTreeMap::new();
    let mut uav_buffer_decls: BTreeMap<u32, (BufferKind, u32)> = BTreeMap::new();
    let mut uav_typed2d_decls: BTreeMap<u32, u32> = BTreeMap::new();
    for decl in &module.decls {
        match decl {
            Sm4Decl::GsInputPrimitive { primitive } => input_primitive = Some(*primitive),
            Sm4Decl::GsOutputTopology { topology } => output_topology = Some(*topology),
            Sm4Decl::GsMaxOutputVertexCount { max } => max_output_vertices = Some(*max),
            Sm4Decl::GsInstanceCount { count } => {
                // Geometry shader instancing (`[instance(n)]` / `dcl_gsinstancecount`) is an SM5
                // feature. Keep the translator resilient by accepting it regardless of module model
                // and clamping invalid values up to 1. If the declaration appears multiple times
                // (some toolchains are redundant), treat the largest value as authoritative.
                let prev = gs_instance_count.unwrap_or(1);
                gs_instance_count = Some(prev.max(*count).max(1));
            }
            Sm4Decl::ConstantBuffer { slot, reg_count } => {
                // Keep the largest declared size so the generated WGSL array is always big enough
                // for any statically indexed reads (cb#[]).
                cbuffer_decls
                    .entry(*slot)
                    .and_modify(|existing| *existing = (*existing).max(*reg_count))
                    .or_insert(*reg_count);
            }
            Sm4Decl::Sampler { slot } => {
                sampler_decls.insert(*slot);
            }
            Sm4Decl::ResourceTexture2D { slot } => {
                texture2d_decls.insert(*slot);
            }
            Sm4Decl::ResourceBuffer { slot, stride, kind } => {
                // Ignore duplicate declarations as long as they agree on buffer kind, keeping the
                // largest stride.
                srv_buffer_decls
                    .entry(*slot)
                    .and_modify(|(existing_kind, existing_stride)| {
                        if *existing_kind == *kind {
                            *existing_stride = (*existing_stride).max(*stride);
                        }
                    })
                    .or_insert((*kind, *stride));
            }
            Sm4Decl::UavBuffer { slot, stride, kind } => {
                // Ignore duplicate declarations as long as they agree on buffer kind, keeping the
                // largest stride.
                uav_buffer_decls
                    .entry(*slot)
                    .and_modify(|(existing_kind, existing_stride)| {
                        if *existing_kind == *kind {
                            *existing_stride = (*existing_stride).max(*stride);
                        }
                    })
                    .or_insert((*kind, *stride));
            }
            Sm4Decl::UavTyped2D { slot, format } => {
                uav_typed2d_decls.insert(*slot, *format);
            }
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
    let mut max_pred_reg: i32 = -1;
    let mut used_cbuffers: BTreeMap<u32, u32> = BTreeMap::new();
    let mut used_textures: BTreeSet<u32> = BTreeSet::new();
    let mut used_samplers: BTreeSet<u32> = BTreeSet::new();
    let mut used_srv_buffers: BTreeSet<u32> = BTreeSet::new();
    let mut used_uav_buffers: BTreeSet<u32> = BTreeSet::new();
    let mut used_uavs_atomic: BTreeSet<u32> = BTreeSet::new();
    let mut used_uav_textures: BTreeMap<u32, StorageTextureFormat> = BTreeMap::new();

    for (inst_index, inst) in module.instructions.iter().enumerate() {
        // Unwrap instruction-level predication so register scanning sees the underlying opcode.
        let mut inst = inst;
        while let Sm4Inst::Predicated { pred, inner } = inst {
            max_pred_reg = max_pred_reg.max(pred.reg.index as i32);
            inst = inner.as_ref();
        }

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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
                )?;
            }
            Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {}
            Sm4Inst::Setp { dst, a, b, .. } => {
                max_pred_reg = max_pred_reg.max(dst.reg.index as i32);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "setp",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::IAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            }
            | Sm4Inst::UAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            } => {
                bump_reg_max(dst_sum.reg, &mut max_temp_reg, &mut max_output_reg);
                bump_reg_max(dst_carry.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        opcode_name(inst),
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::ISubC {
                dst_diff,
                dst_carry,
                a,
                b,
            } => {
                bump_reg_max(dst_diff.reg, &mut max_temp_reg, &mut max_output_reg);
                bump_reg_max(dst_carry.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "isubc",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::USubB {
                dst_diff,
                dst_borrow,
                a,
                b,
            } => {
                bump_reg_max(dst_diff.reg, &mut max_temp_reg, &mut max_output_reg);
                bump_reg_max(dst_borrow.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "usubb",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::UMul {
                dst_lo,
                dst_hi,
                a,
                b,
            }
            | Sm4Inst::IMul {
                dst_lo,
                dst_hi,
                a,
                b,
            } => {
                bump_reg_max(dst_lo.reg, &mut max_temp_reg, &mut max_output_reg);
                if let Some(dst_hi) = dst_hi {
                    bump_reg_max(dst_hi.reg, &mut max_temp_reg, &mut max_output_reg);
                }
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        opcode_name(inst),
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::UMad {
                dst_lo,
                dst_hi,
                a,
                b,
                c,
            }
            | Sm4Inst::IMad {
                dst_lo,
                dst_hi,
                a,
                b,
                c,
            } => {
                bump_reg_max(dst_lo.reg, &mut max_temp_reg, &mut max_output_reg);
                if let Some(dst_hi) = dst_hi {
                    bump_reg_max(dst_hi.reg, &mut max_temp_reg, &mut max_output_reg);
                }
                for src in [a, b, c] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        opcode_name(inst),
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::IAdd { dst, a, b }
            | Sm4Inst::ISub { dst, a, b }
            | Sm4Inst::Or { dst, a, b }
            | Sm4Inst::Xor { dst, a, b }
            | Sm4Inst::IShl { dst, a, b }
            | Sm4Inst::IShr { dst, a, b }
            | Sm4Inst::UShr { dst, a, b }
            | Sm4Inst::IMin { dst, a, b }
            | Sm4Inst::IMax { dst, a, b }
            | Sm4Inst::UMin { dst, a, b }
            | Sm4Inst::UMax { dst, a, b }
            | Sm4Inst::Cmp { dst, a, b, .. } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        opcode_name(inst),
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::Not { dst, src }
            | Sm4Inst::IAbs { dst, src }
            | Sm4Inst::INeg { dst, src }
            | Sm4Inst::Bfrev { dst, src }
            | Sm4Inst::CountBits { dst, src }
            | Sm4Inst::FirstbitHi { dst, src }
            | Sm4Inst::FirstbitLo { dst, src }
            | Sm4Inst::FirstbitShi { dst, src } => {
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
                    &cbuffer_decls,
                    &mut used_cbuffers,
                )?;
            }
            Sm4Inst::UDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            }
            | Sm4Inst::IDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            } => {
                bump_reg_max(dst_quot.reg, &mut max_temp_reg, &mut max_output_reg);
                bump_reg_max(dst_rem.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [a, b] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        opcode_name(inst),
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::Bfi {
                dst,
                width,
                offset,
                insert,
                base,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [width, offset, insert, base] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "bfi",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::Ubfe {
                dst,
                width,
                offset,
                src: src_op,
            }
            | Sm4Inst::Ibfe {
                dst,
                width,
                offset,
                src: src_op,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [width, offset, src_op] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        opcode_name(inst),
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
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
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
            }
            Sm4Inst::Sample {
                dst,
                coord,
                texture,
                sampler,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    coord,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "sample",
                    &input_sivs,
                    &cbuffer_decls,
                    &mut used_cbuffers,
                )?;
                if !texture2d_decls.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "sample",
                        msg: format!(
                            "texture t{} used by sample is missing a dcl_resource_texture2d declaration",
                            texture.slot
                        ),
                    });
                }
                if !sampler_decls.contains(&sampler.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "sample",
                        msg: format!(
                            "sampler s{} used by sample is missing a dcl_sampler declaration",
                            sampler.slot
                        ),
                    });
                }
                if used_srv_buffers.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "sample",
                        msg: format!(
                            "resource slot t{} is used as both a texture and a buffer SRV",
                            texture.slot
                        ),
                    });
                }
                used_textures.insert(texture.slot);
                used_samplers.insert(sampler.slot);
            }
            Sm4Inst::SampleL {
                dst,
                coord,
                texture,
                sampler,
                lod,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [coord, lod] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "sample_l",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
                if !texture2d_decls.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "sample_l",
                        msg: format!(
                            "texture t{} used by sample_l is missing a dcl_resource_texture2d declaration",
                            texture.slot
                        ),
                    });
                }
                if !sampler_decls.contains(&sampler.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "sample_l",
                        msg: format!(
                            "sampler s{} used by sample_l is missing a dcl_sampler declaration",
                            sampler.slot
                        ),
                    });
                }
                if used_srv_buffers.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "sample_l",
                        msg: format!(
                            "resource slot t{} is used as both a texture and a buffer SRV",
                            texture.slot
                        ),
                    });
                }
                used_textures.insert(texture.slot);
                used_samplers.insert(sampler.slot);
            }
            Sm4Inst::ResInfo {
                dst,
                mip_level,
                texture,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    mip_level,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "resinfo",
                    &input_sivs,
                    &cbuffer_decls,
                    &mut used_cbuffers,
                )?;
                if !texture2d_decls.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "resinfo",
                        msg: format!(
                            "texture t{} used by resinfo is missing a dcl_resource_texture2d declaration",
                            texture.slot
                        ),
                    });
                }
                if used_srv_buffers.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "resinfo",
                        msg: format!(
                            "resource slot t{} is used as both a texture and a buffer SRV",
                            texture.slot
                        ),
                    });
                }
                used_textures.insert(texture.slot);
            }
            Sm4Inst::Ld {
                dst,
                coord,
                texture,
                lod,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [coord, lod] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "ld",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
                if !texture2d_decls.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld",
                        msg: format!(
                            "texture t{} used by ld is missing a dcl_resource_texture2d declaration",
                            texture.slot
                        ),
                    });
                }
                if used_srv_buffers.contains(&texture.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld",
                        msg: format!(
                            "resource slot t{} is used as both a texture and a buffer SRV",
                            texture.slot
                        ),
                    });
                }
                used_textures.insert(texture.slot);
            }
            Sm4Inst::LdRaw { dst, addr, buffer } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    addr,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "ld_raw",
                    &input_sivs,
                    &cbuffer_decls,
                    &mut used_cbuffers,
                )?;
                if !srv_buffer_decls.contains_key(&buffer.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_raw",
                        msg: format!(
                            "buffer t{} used by ld_raw is missing a dcl_resource_buffer declaration",
                            buffer.slot
                        ),
                    });
                }
                if used_textures.contains(&buffer.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_raw",
                        msg: format!(
                            "resource slot t{} is used as both a texture and a buffer SRV",
                            buffer.slot
                        ),
                    });
                }
                used_srv_buffers.insert(buffer.slot);
            }
            Sm4Inst::LdUavRaw { dst, addr, uav } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                scan_src_operand(
                    addr,
                    &mut max_temp_reg,
                    &mut max_output_reg,
                    &mut max_gs_input_reg,
                    verts_per_primitive,
                    inst_index,
                    "ld_uav_raw",
                    &input_sivs,
                    &cbuffer_decls,
                    &mut used_cbuffers,
                )?;
                if uav_typed2d_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_uav_raw",
                        msg: format!(
                            "uav slot u{} used by ld_uav_raw is declared as a typed UAV (dcl_uav_typed) but GS prepass only supports UAV buffers",
                            uav.slot
                        ),
                    });
                }
                if !uav_buffer_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_uav_raw",
                        msg: format!(
                            "uav u{} used by ld_uav_raw is missing a dcl_uav_raw/dcl_uav_structured declaration",
                            uav.slot
                        ),
                    });
                }
                used_uav_buffers.insert(uav.slot);
            }
            Sm4Inst::LdStructured {
                dst,
                index,
                offset,
                buffer,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [index, offset] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "ld_structured",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
                if !srv_buffer_decls.contains_key(&buffer.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_structured",
                        msg: format!(
                            "buffer t{} used by ld_structured is missing a dcl_resource_buffer declaration",
                            buffer.slot
                        ),
                    });
                }
                if used_textures.contains(&buffer.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_structured",
                        msg: format!(
                            "resource slot t{} is used as both a texture and a buffer SRV",
                            buffer.slot
                        ),
                    });
                }
                used_srv_buffers.insert(buffer.slot);
            }
            Sm4Inst::LdStructuredUav {
                dst,
                index,
                offset,
                uav,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                for src in [index, offset] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "ld_structured_uav",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
                if uav_typed2d_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_structured_uav",
                        msg: format!(
                            "uav slot u{} used by ld_structured_uav is declared as a typed UAV (dcl_uav_typed) but GS prepass only supports UAV buffers",
                            uav.slot
                        ),
                    });
                }
                let Some((kind, stride)) = uav_buffer_decls.get(&uav.slot).copied() else {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_structured_uav",
                        msg: format!(
                            "uav u{} used by ld_structured_uav is missing a dcl_uav_structured declaration",
                            uav.slot
                        ),
                    });
                };
                if kind != BufferKind::Structured || stride == 0 || (stride % 4) != 0 {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "ld_structured_uav",
                        msg: format!(
                            "uav u{} used by ld_structured_uav must be a structured buffer with stride multiple of 4 (kind={kind:?}, stride={stride})",
                            uav.slot
                        ),
                    });
                }
                used_uav_buffers.insert(uav.slot);
            }
            Sm4Inst::StoreRaw {
                uav,
                addr,
                value,
                mask: _,
            } => {
                for src in [addr, value] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "store_raw",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
                if uav_typed2d_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_raw",
                        msg: format!(
                            "uav slot u{} used by store_raw is declared as a typed UAV (dcl_uav_typed) but GS prepass only supports UAV buffers",
                            uav.slot
                        ),
                    });
                }
                if !uav_buffer_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_raw",
                        msg: format!(
                            "uav u{} used by store_raw is missing a dcl_uav_raw/dcl_uav_structured declaration",
                            uav.slot
                        ),
                    });
                }
                used_uav_buffers.insert(uav.slot);
            }
            Sm4Inst::StoreStructured {
                uav,
                index,
                offset,
                value,
                mask: _,
            } => {
                for src in [index, offset, value] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "store_structured",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
                if uav_typed2d_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_structured",
                        msg: format!(
                            "uav slot u{} used by store_structured is declared as a typed UAV (dcl_uav_typed) but GS prepass only supports UAV buffers",
                            uav.slot
                        ),
                    });
                }
                let Some((kind, stride)) = uav_buffer_decls.get(&uav.slot).copied() else {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_structured",
                        msg: format!(
                            "uav u{} used by store_structured is missing a dcl_uav_structured declaration",
                            uav.slot
                        ),
                    });
                };
                if kind != BufferKind::Structured || stride == 0 || (stride % 4) != 0 {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_structured",
                        msg: format!(
                            "uav u{} used by store_structured must be a structured buffer with stride multiple of 4 (kind={kind:?}, stride={stride})",
                            uav.slot
                        ),
                    });
                }
                used_uav_buffers.insert(uav.slot);
            }
            Sm4Inst::AtomicAdd {
                dst,
                uav,
                addr,
                value,
            } => {
                if let Some(dst) = dst {
                    bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                }
                for src in [addr, value] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "atomic_add",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }
                if uav_typed2d_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "atomic_add",
                        msg: format!(
                            "uav slot u{} used by atomic_add is declared as a typed UAV (dcl_uav_typed) but GS prepass only supports UAV buffers",
                            uav.slot
                        ),
                    });
                }
                if !uav_buffer_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "atomic_add",
                        msg: format!(
                            "uav u{} used by atomic_add is missing a dcl_uav_raw/dcl_uav_structured declaration",
                            uav.slot
                        ),
                    });
                }
                used_uav_buffers.insert(uav.slot);
                used_uavs_atomic.insert(uav.slot);
            }
            Sm4Inst::StoreUavTyped {
                uav,
                coord,
                value,
                mask: _,
            } => {
                for src in [coord, value] {
                    scan_src_operand(
                        src,
                        &mut max_temp_reg,
                        &mut max_output_reg,
                        &mut max_gs_input_reg,
                        verts_per_primitive,
                        inst_index,
                        "store_uav_typed",
                        &input_sivs,
                        &cbuffer_decls,
                        &mut used_cbuffers,
                    )?;
                }

                if uav_buffer_decls.contains_key(&uav.slot) || used_uav_buffers.contains(&uav.slot)
                {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_uav_typed",
                        msg: format!(
                            "uav slot u{} is used as both a UAV buffer and a typed UAV texture",
                            uav.slot
                        ),
                    });
                }

                let Some(&dxgi_format) = uav_typed2d_decls.get(&uav.slot) else {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_uav_typed",
                        msg: format!(
                            "uav u{} used by store_uav_typed is missing a dcl_uav_typed declaration",
                            uav.slot
                        ),
                    });
                };
                let Some(format) = decode_uav_typed2d_format_dxgi(dxgi_format) else {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: "store_uav_typed",
                        msg: format!(
                            "uav u{} used by store_uav_typed has unsupported DXGI format {dxgi_format}",
                            uav.slot
                        ),
                    });
                };
                used_uav_textures.insert(uav.slot, format);
            }
            Sm4Inst::BufInfoRaw { dst, buffer }
            | Sm4Inst::BufInfoStructured { dst, buffer, .. } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                if !srv_buffer_decls.contains_key(&buffer.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: opcode_name(inst),
                        msg: format!(
                            "buffer t{} used by bufinfo is missing a dcl_resource_buffer declaration",
                            buffer.slot
                        ),
                    });
                }
                if used_textures.contains(&buffer.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: opcode_name(inst),
                        msg: format!(
                            "resource slot t{} is used as both a texture and a buffer SRV",
                            buffer.slot
                        ),
                    });
                }
                used_srv_buffers.insert(buffer.slot);
            }
            Sm4Inst::BufInfoRawUav { dst, uav }
            | Sm4Inst::BufInfoStructuredUav {
                dst,
                uav,
                stride_bytes: _,
            } => {
                bump_reg_max(dst.reg, &mut max_temp_reg, &mut max_output_reg);
                if uav_typed2d_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: opcode_name(inst),
                        msg: format!(
                            "uav slot u{} used by bufinfo is declared as a typed UAV (dcl_uav_typed) but GS prepass only supports UAV buffers",
                            uav.slot
                        ),
                    });
                }
                if !uav_buffer_decls.contains_key(&uav.slot) {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode: opcode_name(inst),
                        msg: format!(
                            "uav u{} used by bufinfo is missing a dcl_uav_raw/dcl_uav_structured declaration",
                            uav.slot
                        ),
                    });
                }
                used_uav_buffers.insert(uav.slot);
            }
            Sm4Inst::Emit { stream } => {
                if *stream != 0 {
                    return Err(GsTranslateError::UnsupportedStream {
                        inst_index,
                        opcode: "emit_stream",
                        stream: *stream,
                    });
                }
            }
            Sm4Inst::Cut { stream } => {
                if *stream != 0 {
                    return Err(GsTranslateError::UnsupportedStream {
                        inst_index,
                        opcode: "cut_stream",
                        stream: *stream,
                    });
                }
            }
            Sm4Inst::EmitThenCut { stream } => {
                if *stream != 0 {
                    return Err(GsTranslateError::UnsupportedStream {
                        inst_index,
                        opcode: "emitthen_cut_stream",
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

    // Ensure we always declare at least o0. Some expanded-vertex layouts also need us to declare
    // additional output registers so missing GS outputs can default to `vec4<f32>(0.0)` via the
    // zero-initialized output register file.
    max_output_reg = max_output_reg.max(0);
    let mut emit_output_regs: Vec<u32> = Vec::new();
    match expanded_vertex_layout {
        ExpandedVertexOutputLayout::Packed { varyings } => {
            if let Some(max_varying) = varyings.iter().copied().max() {
                max_output_reg = max_output_reg.max(max_varying as i32);
            }
            emit_output_regs.extend_from_slice(varyings);
        }
        ExpandedVertexOutputLayout::FixedArray { max_varyings } => {
            let max_supported = max_varyings.saturating_sub(1);
            let max_loc = max_output_reg as u32;
            if max_loc > max_supported {
                return Err(GsTranslateError::OutputRegisterOutOfRange {
                    loc: max_loc,
                    max_supported,
                });
            }
            for loc in 1..=max_loc {
                emit_output_regs.push(loc);
            }
        }
    }

    let temp_reg_count = (max_temp_reg + 1).max(0) as u32;
    let output_reg_count = (max_output_reg + 1).max(0) as u32;
    let gs_input_reg_count = (max_gs_input_reg + 1).max(1) as u32;
    let pred_reg_count = (max_pred_reg + 1).max(0) as u32;
    if output_reg_count > EXPANDED_VERTEX_MAX_VARYINGS {
        return Err(GsTranslateError::UnsupportedOutputRegister {
            reg: output_reg_count.saturating_sub(1),
        });
    }

    let mut w = WgslWriter::new();

    w.line("// ---- Aero SM4 geometry shader prepass (generated) ----");
    w.line("");

    // Expanded vertex record written by the compute prepass.
    w.line("struct ExpandedVertex {");
    w.indent();
    w.line("pos: vec4<f32>,");
    match expanded_vertex_layout {
        ExpandedVertexOutputLayout::Packed { varyings } => {
            for (i, _) in varyings.iter().enumerate() {
                w.line(&format!("v{i}: vec4<f32>,"));
            }
        }
        ExpandedVertexOutputLayout::FixedArray { max_varyings } => {
            // Match the emulation passthrough VS (`runtime/wgsl_link.rs`), which expects a fixed-size
            // varying table indexed by `@location(N)`.
            w.line(&format!("varyings: array<vec4<f32>, {max_varyings}>,"));
        }
    }
    w.dedent();
    w.line("};");
    w.line("");

    w.line("struct ExpandedVertexBuffer { data: array<ExpandedVertex> };");
    w.line("struct U32Buffer { data: array<u32> };");
    w.line("struct Vec4F32Buffer { data: array<vec4<f32>> };");
    let needs_u32_struct = !used_srv_buffers.is_empty()
        || used_uav_buffers
            .iter()
            .any(|slot| !used_uavs_atomic.contains(slot));
    let needs_atomic_struct = !used_uavs_atomic.is_empty();
    if needs_u32_struct || needs_atomic_struct {
        // Match the storage-buffer wrapper used by the signature-driven shader translator
        // (`shader_translate.rs`) so SRV/UAV buffer semantics stay consistent across translation
        // paths.
        //
        // WGSL requires storage buffers to have a `struct` as the top-level type; arrays cannot be
        // declared directly as a `var<storage>`.
        if needs_u32_struct {
            w.line("struct AeroStorageBufferU32 { data: array<u32> };");
        }
        if needs_atomic_struct {
            w.line("struct AeroStorageBufferAtomicU32 { data: array<atomic<u32>> };");
        }
        w.line("");
    }

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
    // Pack indirect args and atomic counters into a single storage buffer so the generated WGSL
    // stays within WebGPU's minimum `max_storage_buffers_per_shader_stage` limit (4).
    //
    // Keep the `DrawIndexedIndirectArgs` layout at offset 0 so the executor can feed this buffer
    // directly into `draw_indexed_indirect`.
    //
    // Indirect args + counters share a single storage buffer binding so the translated GS prepass
    // stays within WebGPU's minimum `max_storage_buffers_per_shader_stage` when combined with:
    //   - out_vertices
    //   - out_indices
    //   - gs_inputs.
    w.line("struct GsPrepassState {");
    w.indent();
    w.line("out_indirect: DrawIndexedIndirectArgs,");
    w.line("counters: GsPrepassCounters,");
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

    // Declared constant buffers (`cb#[]`) live in the shared internal/emulation bind group so the
    // compute-emulated geometry stage can reuse the executor's GS resource bindings.
    for (&slot, &reg_count) in &used_cbuffers {
        w.line(&format!(
            "struct Cb{slot} {{ regs: array<vec4<u32>, {reg_count}> }};"
        ));
        w.line(&format!(
            "@group({}) @binding({}) var<uniform> cb{slot}: Cb{slot};",
            BIND_GROUP_INTERNAL_EMULATION,
            BINDING_BASE_CBUFFER + slot
        ));
        w.line("");
    }

    w.line(&format!(
        "@group(0) @binding({}) var<storage, read_write> out_vertices: ExpandedVertexBuffer;",
        GS_PREPASS_BINDING_OUT_VERTICES
    ));
    w.line(&format!(
        "@group(0) @binding({}) var<storage, read_write> out_indices: U32Buffer;",
        GS_PREPASS_BINDING_OUT_INDICES
    ));
    w.line(&format!(
        "@group(0) @binding({}) var<storage, read_write> out_state: GsPrepassState;",
        GS_PREPASS_BINDING_OUT_STATE
    ));
    w.line(&format!(
        "@group(0) @binding({}) var<uniform> params: GsPrepassParams;",
        GS_PREPASS_BINDING_PARAMS
    ));
    // Note: The executor packs multiple storage allocations into the same backing buffer via
    // `ExpansionScratchAllocator`. WebGPU treats STORAGE_READ_WRITE as an exclusive usage at the
    // *buffer* granularity within a dispatch, so if any slice of the backing buffer is bound
    // read/write (our outputs/counters), any other slice must also be bound read/write.
    w.line(&format!(
        "@group(0) @binding({}) var<storage, read_write> gs_inputs: Vec4F32Buffer;",
        GS_PREPASS_BINDING_GS_INPUTS
    ));
    w.line("");

    // D3D stage-ex resources (GS/HS/DS) live in group 3 so we can stay within WebGPU's baseline
    // `maxBindGroups >= 4` guarantee.
    for &slot in &used_textures {
        w.line(&format!(
            "@group({}) @binding({}) var t{slot}: texture_2d<f32>;",
            BIND_GROUP_INTERNAL_EMULATION,
            BINDING_BASE_TEXTURE + slot
        ));
    }
    for &slot in &used_srv_buffers {
        w.line(&format!(
            "@group({}) @binding({}) var<storage, read> t{slot}: AeroStorageBufferU32;",
            BIND_GROUP_INTERNAL_EMULATION,
            BINDING_BASE_TEXTURE + slot
        ));
    }
    if !used_textures.is_empty() || !used_srv_buffers.is_empty() {
        w.line("");
    }
    for &slot in &used_samplers {
        w.line(&format!(
            "@group({}) @binding({}) var s{slot}: sampler;",
            BIND_GROUP_INTERNAL_EMULATION,
            BINDING_BASE_SAMPLER + slot
        ));
    }
    if !used_samplers.is_empty() {
        w.line("");
    }
    for &slot in &used_uav_buffers {
        if used_uavs_atomic.contains(&slot) {
            w.line(&format!(
                "@group({}) @binding({}) var<storage, read_write> u{slot}: AeroStorageBufferAtomicU32;",
                BIND_GROUP_INTERNAL_EMULATION,
                BINDING_BASE_UAV + slot
            ));
        } else {
            w.line(&format!(
                "@group({}) @binding({}) var<storage, read_write> u{slot}: AeroStorageBufferU32;",
                BIND_GROUP_INTERNAL_EMULATION,
                BINDING_BASE_UAV + slot
            ));
        }
    }
    for (&slot, &format) in &used_uav_textures {
        w.line(&format!(
            "@group({}) @binding({}) var u{slot}: texture_storage_2d<{}, write>;",
            BIND_GROUP_INTERNAL_EMULATION,
            BINDING_BASE_UAV + slot,
            format.wgsl_format()
        ));
    }
    if !used_uav_buffers.is_empty() || !used_uav_textures.is_empty() {
        w.line("");
    }

    // GS input helper (v#[]).
    w.line(
        "fn gs_load_input(draw_instance_id: u32, prim_id: u32, reg: u32, vertex: u32) -> vec4<f32> {",
    );
    w.indent();
    w.line("// Flattened index:");
    w.line(
        "// (((draw_instance_id * primitive_count + prim_id) * verts_per_prim + vertex) * reg_count + reg).",
    );
    w.line("let idx = (((draw_instance_id * params.primitive_count + prim_id) * GS_INPUT_VERTS_PER_PRIM + vertex) * GS_INPUT_REG_COUNT + reg);");
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

    // Emit semantics: append a vertex and produce list indices based on the GS output topology
    // (point list, line strip, triangle strip).
    w.line("fn gs_emit(");
    w.indent();
    w.line("o0: vec4<f32>,");
    for &loc in &emit_output_regs {
        w.line(&format!("o{loc}: vec4<f32>,"));
    }
    w.line("emitted_count: ptr<function, u32>,");
    w.line("strip_len: ptr<function, u32>,");
    w.line("strip_prev0: ptr<function, u32>,");
    w.line("strip_prev1: ptr<function, u32>,");
    w.line("overflow: ptr<function, bool>,");
    w.dedent();
    w.line(") {");
    w.indent();
    w.line("if (*overflow) { return; }");
    w.line("if (atomicLoad(&out_state.counters.overflow) != 0u) { *overflow = true; return; }");
    w.line("if (*emitted_count >= GS_MAX_VERTEX_COUNT) { return; }");
    w.line("");
    w.line("let vtx_idx = atomicAdd(&out_state.counters.vertex_count, 1u);");
    w.line("let vtx_cap = arrayLength(&out_vertices.data);");
    w.line("if (vtx_idx >= vtx_cap) {");
    w.indent();
    w.line("atomicOr(&out_state.counters.overflow, 1u);");
    w.line("*overflow = true;");
    w.line("return;");
    w.dedent();
    w.line("}");
    w.line("");
    w.line("out_vertices.data[vtx_idx].pos = o0;");
    match expanded_vertex_layout {
        ExpandedVertexOutputLayout::Packed { .. } => {
            for (i, &loc) in emit_output_regs.iter().enumerate() {
                w.line(&format!("out_vertices.data[vtx_idx].v{i} = o{loc};"));
            }
        }
        ExpandedVertexOutputLayout::FixedArray { max_varyings } => {
            w.line(&format!(
                "out_vertices.data[vtx_idx].varyings = array<vec4<f32>, {max_varyings}>();"
            ));
            for &loc in &emit_output_regs {
                w.line(&format!(
                    "out_vertices.data[vtx_idx].varyings[{loc}u] = o{loc};"
                ));
            }
        }
    }
    w.line("");
    match output_topology_kind {
        GsOutputTopologyKind::PointList => {
            w.line("// Point list index emission.");
            w.line("let base = atomicAdd(&out_state.counters.index_count, 1u);");
            w.line("let idx_cap = arrayLength(&out_indices.data);");
            w.line("if (base >= idx_cap) {");
            w.indent();
            w.line("atomicOr(&out_state.counters.overflow, 1u);");
            w.line("*overflow = true;");
            w.line("return;");
            w.dedent();
            w.line("}");
            w.line("out_indices.data[base] = vtx_idx;");
        }
        GsOutputTopologyKind::LineStrip => {
            w.line("// Line strip -> line list index emission.");
            w.line("if (*strip_len == 0u) {");
            w.indent();
            w.line("*strip_prev0 = vtx_idx;");
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line("let base = atomicAdd(&out_state.counters.index_count, 2u);");
            w.line("let idx_cap = arrayLength(&out_indices.data);");
            w.line("if (base + 1u >= idx_cap) {");
            w.indent();
            w.line("atomicOr(&out_state.counters.overflow, 1u);");
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
        GsOutputTopologyKind::TriangleStrip => {
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
            w.line("let base = atomicAdd(&out_state.counters.index_count, 3u);");
            w.line("let idx_cap = arrayLength(&out_indices.data);");
            w.line("if (base + 2u >= idx_cap) {");
            w.indent();
            w.line("atomicOr(&out_state.counters.overflow, 1u);");
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
    w.line("draw_instance_id_in: u32,");
    w.line("prim_id: u32,");
    w.line("gs_instance_id_in: u32,");
    w.line("overflow: ptr<function, bool>,");
    w.dedent();
    w.line(") {");
    w.indent();
    w.line("if (*overflow) { return; }");
    w.line("if (atomicLoad(&out_state.counters.overflow) != 0u) { *overflow = true; return; }");
    w.line("");

    for i in 0..temp_reg_count {
        w.line(&format!("var r{i}: vec4<f32> = vec4<f32>(0.0);"));
    }
    for i in 0..pred_reg_count {
        w.line(&format!("var p{i}: vec4<bool> = vec4<bool>(false);"));
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
    w.line("let draw_instance_id: u32 = draw_instance_id_in;"); // Used for v#[] lookup.
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
        // Compare-based flow control uses the same D3D10/11 tokenized-program comparison encoding
        // as `setp`: `*_U` variants are unordered float comparisons and are true when either
        // operand is NaN.
        let a_vec = emit_src_vec4(inst_index, opcode, a, &input_sivs)?;
        let b_vec = emit_src_vec4(inst_index, opcode, b, &input_sivs)?;
        let a = format!("({a_vec}).x");
        let b = format!("({b_vec}).x");
        Ok(emit_sm4_cmp_op_scalar_bool(op, &a, &b))
    };

    let mut gs_emit_args = String::from("o0");
    for &loc in &emit_output_regs {
        gs_emit_args.push_str(&format!(", o{loc}"));
    }
    gs_emit_args.push_str(", &emitted_count, &strip_len, &strip_prev0, &strip_prev1, overflow");

    for (inst_index, inst) in module.instructions.iter().enumerate() {
        // Unwrap instruction-level predication (`(+p0.x) mov ...`).
        let mut inst = inst;
        let mut predicates: Vec<PredicateOperand> = Vec::new();
        while let Sm4Inst::Predicated { pred, inner } = inst {
            predicates.push(*pred);
            inst = inner.as_ref();
        }

        match inst {
            Sm4Inst::Case { value } => {
                if !predicates.is_empty() {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_case",
                    });
                }

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
                if !predicates.is_empty() {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_default",
                    });
                }

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
                if !predicates.is_empty() {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_endswitch",
                    });
                }

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

        // Emit predicated instructions as an `if` wrapper around the inner opcode.
        if !predicates.is_empty() {
            match inst {
                Sm4Inst::If { .. } => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_if",
                    })
                }
                Sm4Inst::IfC { .. } => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_ifc",
                    })
                }
                Sm4Inst::Else => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_else",
                    })
                }
                Sm4Inst::EndIf => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_endif",
                    })
                }
                Sm4Inst::Loop => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_loop",
                    })
                }
                Sm4Inst::EndLoop => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_endloop",
                    })
                }
                Sm4Inst::Switch { .. } => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_switch",
                    })
                }
                Sm4Inst::Ret => {
                    return Err(GsTranslateError::UnsupportedInstruction {
                        inst_index,
                        opcode: "predicated_ret",
                    })
                }
                Sm4Inst::Case { .. } | Sm4Inst::Default | Sm4Inst::EndSwitch => {
                    unreachable!("switch label opcodes handled above")
                }
                _ => {}
            }

            for pred in &predicates {
                let cond = emit_test_predicate_scalar(pred);
                w.line(&format!("if ({cond}) {{"));
                w.indent();
            }
        }

        let r = (|| -> Result<(), GsTranslateError> {
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
                    let selector_i =
                        emit_src_vec4_i32(inst_index, "switch", selector, &input_sivs)?;
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
                Sm4Inst::Setp { dst, op, a, b } => {
                    let setp_a = format!("setp_a_{inst_index}");
                    let setp_b = format!("setp_b_{inst_index}");
                    let setp_cmp = format!("setp_cmp_{inst_index}");
                    let a_expr = emit_src_vec4(inst_index, "setp", a, &input_sivs)?;
                    let b_expr = emit_src_vec4(inst_index, "setp", b, &input_sivs)?;
                    w.line(&format!("let {setp_a} = {a_expr};"));
                    w.line(&format!("let {setp_b} = {b_expr};"));
                    w.line(&format!(
                        "let {setp_cmp} = {};",
                        emit_sm4_cmp_op_vec4_bool(*op, &setp_a, &setp_b)
                    ));
                    emit_write_masked_bool(&mut w, *dst, setp_cmp)?;
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
                    // `f16tof32` consumes a raw IEEE 754 binary16 payload stored in the low 16 bits
                    // of each untyped register lane.
                    //
                    // Operand modifiers apply to the numeric `f32` result of the conversion, not
                    // the raw half-float payload. Preserve the raw half bits while reading the
                    // operand, then apply modifiers after unpacking.
                    let mut src_bits = src.clone();
                    src_bits.modifier = OperandModifier::None;
                    let src_u = emit_src_vec4_u32(inst_index, "f16tof32", &src_bits, &input_sivs)?;

                    let unpack_lane =
                        |c: char| format!("unpack2x16float((({src_u}).{c} & 0xffffu)).x");
                    let x = unpack_lane('x');
                    let y = unpack_lane('y');
                    let z = unpack_lane('z');
                    let w_lane = unpack_lane('w');

                    let rhs = format!("vec4<f32>({x}, {y}, {z}, {w_lane})");
                    let rhs = apply_modifier(rhs, src.modifier);
                    let rhs = maybe_saturate(dst.saturate, rhs);
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
                Sm4Inst::IAddC {
                    dst_sum,
                    dst_carry,
                    a,
                    b,
                } => {
                    emit_add_with_carry(
                        &mut w,
                        "iaddc",
                        inst_index,
                        dst_sum,
                        dst_carry,
                        a,
                        b,
                        &input_sivs,
                    )?;
                }
                Sm4Inst::UAddC {
                    dst_sum,
                    dst_carry,
                    a,
                    b,
                } => {
                    emit_add_with_carry(
                        &mut w,
                        "uaddc",
                        inst_index,
                        dst_sum,
                        dst_carry,
                        a,
                        b,
                        &input_sivs,
                    )?;
                }
                Sm4Inst::ISubC {
                    dst_diff,
                    dst_carry,
                    a,
                    b,
                } => {
                    emit_sub_with_carry(
                        &mut w,
                        "isubc",
                        inst_index,
                        dst_diff,
                        dst_carry,
                        a,
                        b,
                        &input_sivs,
                    )?;
                }
                Sm4Inst::USubB {
                    dst_diff,
                    dst_borrow,
                    a,
                    b,
                } => {
                    emit_sub_with_borrow(
                        &mut w,
                        "usubb",
                        inst_index,
                        dst_diff,
                        dst_borrow,
                        a,
                        b,
                        &input_sivs,
                    )?;
                }
                Sm4Inst::And { dst, a, b } => {
                    let a = emit_src_vec4_u32(inst_index, "and", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "and", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(({a}) & ({b}))");
                    emit_write_masked(&mut w, inst_index, "and", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::UMul {
                    dst_lo,
                    dst_hi,
                    a,
                    b,
                } => {
                    let a = emit_src_vec4_u32(inst_index, "umul", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "umul", b, &input_sivs)?;
                    let lo = format!("bitcast<vec4<f32>>((({a}) * ({b})))");
                    emit_write_masked(&mut w, inst_index, "umul", dst_lo.reg, dst_lo.mask, lo)?;

                    if let Some(dst_hi) = dst_hi {
                        let hi_u = emit_u32_mul_hi(&a, &b);
                        let hi = format!("bitcast<vec4<f32>>({hi_u})");
                        emit_write_masked(&mut w, inst_index, "umul", dst_hi.reg, dst_hi.mask, hi)?;
                    }
                }
                Sm4Inst::IMul {
                    dst_lo,
                    dst_hi,
                    a,
                    b,
                } => {
                    let a = emit_src_vec4_i32(inst_index, "imul", a, &input_sivs)?;
                    let b = emit_src_vec4_i32(inst_index, "imul", b, &input_sivs)?;
                    let lo = format!("bitcast<vec4<f32>>((({a}) * ({b})))");
                    emit_write_masked(&mut w, inst_index, "imul", dst_lo.reg, dst_lo.mask, lo)?;

                    if let Some(dst_hi) = dst_hi {
                        let hi_i = emit_i32_mul_hi(&a, &b);
                        let hi = format!("bitcast<vec4<f32>>({hi_i})");
                        emit_write_masked(&mut w, inst_index, "imul", dst_hi.reg, dst_hi.mask, hi)?;
                    }
                }
                Sm4Inst::UMad {
                    dst_lo,
                    dst_hi,
                    a,
                    b,
                    c,
                } => {
                    let a = emit_src_vec4_u32(inst_index, "umad", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "umad", b, &input_sivs)?;
                    let c = emit_src_vec4_u32(inst_index, "umad", c, &input_sivs)?;
                    let lo = format!("bitcast<vec4<f32>>((({a}) * ({b}) + ({c})))");
                    emit_write_masked(&mut w, inst_index, "umad", dst_lo.reg, dst_lo.mask, lo)?;

                    if let Some(dst_hi) = dst_hi {
                        let hi_u = emit_u32_mad_hi(&a, &b, &c);
                        let hi = format!("bitcast<vec4<f32>>({hi_u})");
                        emit_write_masked(&mut w, inst_index, "umad", dst_hi.reg, dst_hi.mask, hi)?;
                    }
                }
                Sm4Inst::IMad {
                    dst_lo,
                    dst_hi,
                    a,
                    b,
                    c,
                } => {
                    let a = emit_src_vec4_i32(inst_index, "imad", a, &input_sivs)?;
                    let b = emit_src_vec4_i32(inst_index, "imad", b, &input_sivs)?;
                    let c = emit_src_vec4_i32(inst_index, "imad", c, &input_sivs)?;
                    let lo = format!("bitcast<vec4<f32>>((({a}) * ({b}) + ({c})))");
                    emit_write_masked(&mut w, inst_index, "imad", dst_lo.reg, dst_lo.mask, lo)?;

                    if let Some(dst_hi) = dst_hi {
                        let hi_i = emit_i32_mad_hi(&a, &b, &c);
                        let hi = format!("bitcast<vec4<f32>>({hi_i})");
                        emit_write_masked(&mut w, inst_index, "imad", dst_hi.reg, dst_hi.mask, hi)?;
                    }
                }
                Sm4Inst::IAdd { dst, a, b } => {
                    let a = emit_src_vec4_i32(inst_index, "iadd", a, &input_sivs)?;
                    let b = emit_src_vec4_i32(inst_index, "iadd", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(({a}) + ({b}))");
                    emit_write_masked(&mut w, inst_index, "iadd", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::ISub { dst, a, b } => {
                    let a = emit_src_vec4_i32(inst_index, "isub", a, &input_sivs)?;
                    let b = emit_src_vec4_i32(inst_index, "isub", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(({a}) - ({b}))");
                    emit_write_masked(&mut w, inst_index, "isub", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Or { dst, a, b } => {
                    let a = emit_src_vec4_u32(inst_index, "or", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "or", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(({a}) | ({b}))");
                    emit_write_masked(&mut w, inst_index, "or", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Xor { dst, a, b } => {
                    let a = emit_src_vec4_u32(inst_index, "xor", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "xor", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(({a}) ^ ({b}))");
                    emit_write_masked(&mut w, inst_index, "xor", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Not { dst, src } => {
                    let src = emit_src_vec4_u32(inst_index, "not", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(~({src}))");
                    emit_write_masked(&mut w, inst_index, "not", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::IShl { dst, a, b } => {
                    let a = emit_src_vec4_u32(inst_index, "ishl", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "ishl", b, &input_sivs)?;
                    // DXBC shift ops mask the shift amount to 0..31 (lower 5 bits).
                    let sh = format!("({b}) & vec4<u32>(31u)");
                    let rhs = format!("bitcast<vec4<f32>>(({a}) << ({sh}))");
                    emit_write_masked(&mut w, inst_index, "ishl", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::IShr { dst, a, b } => {
                    let a = emit_src_vec4_i32(inst_index, "ishr", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "ishr", b, &input_sivs)?;
                    // DXBC shift ops mask the shift amount to 0..31 (lower 5 bits).
                    let sh = format!("({b}) & vec4<u32>(31u)");
                    let rhs = format!("bitcast<vec4<f32>>(({a}) >> ({sh}))");
                    emit_write_masked(&mut w, inst_index, "ishr", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::UShr { dst, a, b } => {
                    let a = emit_src_vec4_u32(inst_index, "ushr", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "ushr", b, &input_sivs)?;
                    // DXBC shift ops mask the shift amount to 0..31 (lower 5 bits).
                    let sh = format!("({b}) & vec4<u32>(31u)");
                    let rhs = format!("bitcast<vec4<f32>>(({a}) >> ({sh}))");
                    emit_write_masked(&mut w, inst_index, "ushr", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::IMin { dst, a, b } => {
                    let a = emit_src_vec4_i32(inst_index, "imin", a, &input_sivs)?;
                    let b = emit_src_vec4_i32(inst_index, "imin", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(vec4<u32>(min(({a}), ({b}))))");
                    emit_write_masked(&mut w, inst_index, "imin", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::IMax { dst, a, b } => {
                    let a = emit_src_vec4_i32(inst_index, "imax", a, &input_sivs)?;
                    let b = emit_src_vec4_i32(inst_index, "imax", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(vec4<u32>(max(({a}), ({b}))))");
                    emit_write_masked(&mut w, inst_index, "imax", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::UMin { dst, a, b } => {
                    let a = emit_src_vec4_u32(inst_index, "umin", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "umin", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(min(({a}), ({b})))");
                    emit_write_masked(&mut w, inst_index, "umin", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::UMax { dst, a, b } => {
                    let a = emit_src_vec4_u32(inst_index, "umax", a, &input_sivs)?;
                    let b = emit_src_vec4_u32(inst_index, "umax", b, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(max(({a}), ({b})))");
                    emit_write_masked(&mut w, inst_index, "umax", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::IAbs { dst, src } => {
                    let src = emit_src_vec4_i32(inst_index, "iabs", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(vec4<u32>(abs({src})))");
                    emit_write_masked(&mut w, inst_index, "iabs", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::INeg { dst, src } => {
                    let src = emit_src_vec4_i32(inst_index, "ineg", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(vec4<u32>(-({src})))");
                    emit_write_masked(&mut w, inst_index, "ineg", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Cmp { dst, a, b, op, ty } => {
                    let opcode = "cmp";

                    let cmp = match ty {
                        CmpType::F32 => {
                            // D3D float compare opcodes are ordered. In particular, ordered `ne`
                            // must be false when either operand is NaN (unlike WGSL `!=`).
                            //
                            // Reuse `Sm4CmpOp` semantics (shared with `setp`/`ifc`).
                            let a_expr = emit_src_vec4(inst_index, opcode, a, &input_sivs)?;
                            let b_expr = emit_src_vec4(inst_index, opcode, b, &input_sivs)?;

                            // Avoid repeating potentially complex src expressions per-lane.
                            let a_var = format!("cmp_a_{inst_index}");
                            let b_var = format!("cmp_b_{inst_index}");
                            let cmp_var = format!("cmp_cond_{inst_index}");
                            w.line(&format!("let {a_var} = {a_expr};"));
                            w.line(&format!("let {b_var} = {b_expr};"));

                            let sm4_op = match op {
                                CmpOp::Eq => Sm4CmpOp::Eq,
                                CmpOp::Ne => Sm4CmpOp::Ne,
                                CmpOp::Lt => Sm4CmpOp::Lt,
                                CmpOp::Le => Sm4CmpOp::Le,
                                CmpOp::Gt => Sm4CmpOp::Gt,
                                CmpOp::Ge => Sm4CmpOp::Ge,
                            };
                            let cmp_expr = emit_sm4_cmp_op_vec4_bool(sm4_op, &a_var, &b_var);
                            w.line(&format!("let {cmp_var} = {cmp_expr};"));
                            cmp_var
                        }
                        CmpType::I32 => {
                            let a = emit_src_vec4_i32(inst_index, opcode, a, &input_sivs)?;
                            let b = emit_src_vec4_i32(inst_index, opcode, b, &input_sivs)?;
                            match op {
                                CmpOp::Eq => format!("({a}) == ({b})"),
                                CmpOp::Ne => format!("({a}) != ({b})"),
                                CmpOp::Lt => format!("({a}) < ({b})"),
                                CmpOp::Le => format!("({a}) <= ({b})"),
                                CmpOp::Gt => format!("({a}) > ({b})"),
                                CmpOp::Ge => format!("({a}) >= ({b})"),
                            }
                        }
                        CmpType::U32 => {
                            let a = emit_src_vec4_u32(inst_index, opcode, a, &input_sivs)?;
                            let b = emit_src_vec4_u32(inst_index, opcode, b, &input_sivs)?;
                            match op {
                                CmpOp::Eq => format!("({a}) == ({b})"),
                                CmpOp::Ne => format!("({a}) != ({b})"),
                                CmpOp::Lt => format!("({a}) < ({b})"),
                                CmpOp::Le => format!("({a}) <= ({b})"),
                                CmpOp::Gt => format!("({a}) > ({b})"),
                                CmpOp::Ge => format!("({a}) >= ({b})"),
                            }
                        }
                    };

                    // Convert the bool vector result into D3D-style predicate mask bits.
                    let mask = format!("select(vec4<u32>(0u), vec4<u32>(0xffffffffu), {cmp})");
                    let rhs = format!("bitcast<vec4<f32>>({mask})");
                    emit_write_masked(&mut w, inst_index, opcode, dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::UDiv {
                    dst_quot,
                    dst_rem,
                    a,
                    b,
                } => {
                    // DXBC integer ops write raw bits into the untyped register file. Model that by
                    // bitcasting through `u32`, performing the arithmetic, then bitcasting back to
                    // `f32` before writing.
                    let a_u = emit_src_vec4_u32(inst_index, "udiv", a, &input_sivs)?;
                    let b_u = emit_src_vec4_u32(inst_index, "udiv", b, &input_sivs)?;
                    let a_name = format!("udiv_a{inst_index}");
                    let b_name = format!("udiv_b{inst_index}");
                    let q_name = format!("udiv_q{inst_index}");
                    let r_name = format!("udiv_r{inst_index}");
                    let q_f_name = format!("udiv_qf{inst_index}");
                    let r_f_name = format!("udiv_rf{inst_index}");
                    w.line(&format!("let {a_name}: vec4<u32> = {a_u};"));
                    w.line(&format!("let {b_name}: vec4<u32> = {b_u};"));
                    w.line(&format!(
                        "let {q_name}: vec4<u32> = ({a_name}) / ({b_name});"
                    ));
                    w.line(&format!(
                        "let {r_name}: vec4<u32> = ({a_name}) % ({b_name});"
                    ));
                    w.line(&format!(
                        "let {q_f_name}: vec4<f32> = bitcast<vec4<f32>>({q_name});"
                    ));
                    w.line(&format!(
                        "let {r_f_name}: vec4<f32> = bitcast<vec4<f32>>({r_name});"
                    ));
                    emit_write_masked(
                        &mut w,
                        inst_index,
                        "udiv",
                        dst_quot.reg,
                        dst_quot.mask,
                        q_f_name,
                    )?;
                    emit_write_masked(
                        &mut w,
                        inst_index,
                        "udiv",
                        dst_rem.reg,
                        dst_rem.mask,
                        r_f_name,
                    )?;
                }
                Sm4Inst::IDiv {
                    dst_quot,
                    dst_rem,
                    a,
                    b,
                } => {
                    // Same idea as `udiv`, but operate on signed integers.
                    let a_i = emit_src_vec4_i32(inst_index, "idiv", a, &input_sivs)?;
                    let b_i = emit_src_vec4_i32(inst_index, "idiv", b, &input_sivs)?;
                    let a_name = format!("idiv_a{inst_index}");
                    let b_name = format!("idiv_b{inst_index}");
                    let q_name = format!("idiv_q{inst_index}");
                    let r_name = format!("idiv_r{inst_index}");
                    let q_f_name = format!("idiv_qf{inst_index}");
                    let r_f_name = format!("idiv_rf{inst_index}");
                    w.line(&format!("let {a_name}: vec4<i32> = {a_i};"));
                    w.line(&format!("let {b_name}: vec4<i32> = {b_i};"));
                    w.line(&format!(
                        "let {q_name}: vec4<i32> = ({a_name}) / ({b_name});"
                    ));
                    w.line(&format!(
                        "let {r_name}: vec4<i32> = ({a_name}) % ({b_name});"
                    ));
                    w.line(&format!(
                        "let {q_f_name}: vec4<f32> = bitcast<vec4<f32>>({q_name});"
                    ));
                    w.line(&format!(
                        "let {r_f_name}: vec4<f32> = bitcast<vec4<f32>>({r_name});"
                    ));
                    emit_write_masked(
                        &mut w,
                        inst_index,
                        "idiv",
                        dst_quot.reg,
                        dst_quot.mask,
                        q_f_name,
                    )?;
                    emit_write_masked(
                        &mut w,
                        inst_index,
                        "idiv",
                        dst_rem.reg,
                        dst_rem.mask,
                        r_f_name,
                    )?;
                }
                Sm4Inst::Bfi {
                    dst,
                    width,
                    offset,
                    insert,
                    base,
                } => {
                    let width_i = emit_src_vec4_i32(inst_index, "bfi", width, &input_sivs)?;
                    let offset_i = emit_src_vec4_i32(inst_index, "bfi", offset, &input_sivs)?;
                    let insert_i = emit_src_vec4_i32(inst_index, "bfi", insert, &input_sivs)?;
                    let base_i = emit_src_vec4_i32(inst_index, "bfi", base, &input_sivs)?;

                    // WGSL `insertBits` takes scalar `offset`/`count`, but DXBC operands are vectors.
                    // Emit per-lane inserts so swizzles (common in pack/unpack patterns) behave like
                    // DXBC.
                    let lanes = ['x', 'y', 'z', 'w'];
                    let mut out = Vec::with_capacity(4);
                    for lane in lanes {
                        let offset_u = format!("u32(({offset_i}).{lane})");
                        let count_u = format!("u32(({width_i}).{lane})");
                        let insert_u = format!("bitcast<u32>(({insert_i}).{lane})");
                        let base_u = format!("bitcast<u32>(({base_i}).{lane})");
                        out.push(format!(
                            "bitcast<f32>(insertBits({base_u}, {insert_u}, {offset_u}, {count_u}))"
                        ));
                    }
                    let rhs = format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3]);
                    emit_write_masked(&mut w, inst_index, "bfi", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Ubfe {
                    dst,
                    width,
                    offset,
                    src,
                } => {
                    let width_i = emit_src_vec4_i32(inst_index, "ubfe", width, &input_sivs)?;
                    let offset_i = emit_src_vec4_i32(inst_index, "ubfe", offset, &input_sivs)?;
                    let src_i = emit_src_vec4_i32(inst_index, "ubfe", src, &input_sivs)?;

                    let lanes = ['x', 'y', 'z', 'w'];
                    let mut out = Vec::with_capacity(4);
                    for lane in lanes {
                        let offset_u = format!("u32(({offset_i}).{lane})");
                        let count_u = format!("u32(({width_i}).{lane})");
                        let src_u = format!("bitcast<u32>(({src_i}).{lane})");
                        out.push(format!(
                            "bitcast<f32>(extractBits({src_u}, {offset_u}, {count_u}))"
                        ));
                    }
                    let rhs = format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3]);
                    emit_write_masked(&mut w, inst_index, "ubfe", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Ibfe {
                    dst,
                    width,
                    offset,
                    src,
                } => {
                    let width_i = emit_src_vec4_i32(inst_index, "ibfe", width, &input_sivs)?;
                    let offset_i = emit_src_vec4_i32(inst_index, "ibfe", offset, &input_sivs)?;
                    let src_i = emit_src_vec4_i32(inst_index, "ibfe", src, &input_sivs)?;

                    // `extractBits(i32, ...)` sign-extends in WGSL, matching D3D's `ibfe`.
                    let lanes = ['x', 'y', 'z', 'w'];
                    let mut out = Vec::with_capacity(4);
                    for lane in lanes {
                        let offset_u = format!("u32(({offset_i}).{lane})");
                        let count_u = format!("u32(({width_i}).{lane})");
                        let src_s = format!("({src_i}).{lane}");
                        out.push(format!(
                            "bitcast<f32>(extractBits({src_s}, {offset_u}, {count_u}))"
                        ));
                    }
                    let rhs = format!("vec4<f32>({}, {}, {}, {})", out[0], out[1], out[2], out[3]);
                    emit_write_masked(&mut w, inst_index, "ibfe", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Bfrev { dst, src } => {
                    let src_u = emit_src_vec4_u32(inst_index, "bfrev", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(reverseBits({src_u}))");
                    emit_write_masked(&mut w, inst_index, "bfrev", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::CountBits { dst, src } => {
                    let src_u = emit_src_vec4_u32(inst_index, "countbits", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(countOneBits({src_u}))");
                    emit_write_masked(&mut w, inst_index, "countbits", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::FirstbitHi { dst, src } => {
                    let src_u = emit_src_vec4_u32(inst_index, "firstbit_hi", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(firstLeadingBit({src_u}))");
                    emit_write_masked(&mut w, inst_index, "firstbit_hi", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::FirstbitLo { dst, src } => {
                    let src_u = emit_src_vec4_u32(inst_index, "firstbit_lo", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(firstTrailingBit({src_u}))");
                    emit_write_masked(&mut w, inst_index, "firstbit_lo", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::FirstbitShi { dst, src } => {
                    let src_i = emit_src_vec4_i32(inst_index, "firstbit_shi", src, &input_sivs)?;
                    let rhs = format!("bitcast<vec4<f32>>(firstLeadingBit({src_i}))");
                    emit_write_masked(&mut w, inst_index, "firstbit_shi", dst.reg, dst.mask, rhs)?;
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
                Sm4Inst::Sample {
                    dst,
                    coord,
                    texture,
                    sampler,
                } => {
                    let coord = emit_src_vec4(inst_index, "sample", coord, &input_sivs)?;
                    // The translated GS prepass always runs as a compute shader, so use explicit LOD
                    // sampling to keep the generated WGSL valid outside the fragment stage.
                    let rhs = format!(
                        "textureSampleLevel(t{}, s{}, ({coord}).xy, 0.0)",
                        texture.slot, sampler.slot
                    );
                    let rhs = maybe_saturate(dst.saturate, rhs);
                    emit_write_masked(&mut w, inst_index, "sample", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::SampleL {
                    dst,
                    coord,
                    texture,
                    sampler,
                    lod,
                } => {
                    let coord = emit_src_vec4(inst_index, "sample_l", coord, &input_sivs)?;
                    let lod_vec = emit_src_vec4(inst_index, "sample_l", lod, &input_sivs)?;
                    let rhs = format!(
                        "textureSampleLevel(t{}, s{}, ({coord}).xy, ({lod_vec}).x)",
                        texture.slot, sampler.slot
                    );
                    let rhs = maybe_saturate(dst.saturate, rhs);
                    emit_write_masked(&mut w, inst_index, "sample_l", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::ResInfo {
                    dst,
                    mip_level,
                    texture,
                } => {
                    // `resinfo` is used by `Texture2D.GetDimensions` and produces integer values.
                    //
                    // Output packing for `Texture2D`:
                    // - x = width
                    // - y = height
                    // - z = 1
                    // - w = mip level count
                    //
                    // DXBC register files are untyped; store the raw `u32` bits into our `vec4<f32>`
                    // register model via a bitcast.
                    let mip_u = emit_src_vec4_u32(inst_index, "resinfo", mip_level, &input_sivs)?;
                    let level_i = format!("i32(({mip_u}).x)");
                    let dims_name = format!("resinfo_dims{inst_index}");
                    w.line(&format!(
                        "let {dims_name}: vec2<u32> = textureDimensions(t{}, {level_i});",
                        texture.slot
                    ));
                    let levels_name = format!("resinfo_levels{inst_index}");
                    w.line(&format!(
                        "let {levels_name}: u32 = textureNumLevels(t{});",
                        texture.slot
                    ));
                    let rhs = format!(
                        "bitcast<vec4<f32>>(vec4<u32>(({dims_name}).x, ({dims_name}).y, 1u, {levels_name}))"
                    );
                    emit_write_masked(&mut w, inst_index, "resinfo", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Ld {
                    dst,
                    coord,
                    texture,
                    lod,
                } => {
                    // SM4 `ld` (e.g. `Texture2D.Load`) consumes integer texel coordinates and an
                    // integer mip level. Interpret the source lanes strictly as integer bits.
                    let coord_i = emit_src_vec4_i32(inst_index, "ld", coord, &input_sivs)?;
                    let x = format!("({coord_i}).x");
                    let y = format!("({coord_i}).y");
                    let lod_i = emit_src_vec4_i32(inst_index, "ld", lod, &input_sivs)?;
                    let lod_scalar = format!("({lod_i}).x");
                    let rhs = format!(
                        "textureLoad(t{}, vec2<i32>({x}, {y}), {lod_scalar})",
                        texture.slot
                    );
                    let rhs = maybe_saturate(dst.saturate, rhs);
                    emit_write_masked(&mut w, inst_index, "ld", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::LdRaw { dst, addr, buffer } => {
                    // Raw buffer loads operate on byte offsets. Model buffers as a storage `array<u32>`
                    // and derive a word index from the byte address.
                    let addr_u32 =
                        emit_src_scalar_u32_addr(inst_index, "ld_raw", addr, &input_sivs)?;
                    let base_name = format!("ld_raw_base{inst_index}");
                    w.line(&format!("let {base_name}: u32 = ({addr_u32}) / 4u;"));

                    let mask_bits = dst.mask.0 & 0xF;
                    let load_lane = |bit: u8, offset: u32| {
                        if (mask_bits & bit) != 0 {
                            format!("t{}.data[{base_name} + {offset}u]", buffer.slot)
                        } else {
                            "0u".to_owned()
                        }
                    };

                    let u_name = format!("ld_raw_u{inst_index}");
                    w.line(&format!(
                        "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                        load_lane(1, 0),
                        load_lane(2, 1),
                        load_lane(4, 2),
                        load_lane(8, 3),
                    ));
                    let f_name = format!("ld_raw_f{inst_index}");
                    w.line(&format!(
                        "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                    ));

                    let rhs = maybe_saturate(dst.saturate, f_name);
                    emit_write_masked(&mut w, inst_index, "ld_raw", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::LdUavRaw { dst, addr, uav } => {
                    // Raw UAV loads operate on byte offsets. Model UAV buffers as a storage
                    // `array<u32>` and derive a word index from the byte address.
                    let addr_u32 =
                        emit_src_scalar_u32_addr(inst_index, "ld_uav_raw", addr, &input_sivs)?;
                    let base_name = format!("ld_uav_raw_base{inst_index}");
                    w.line(&format!("let {base_name}: u32 = ({addr_u32}) / 4u;"));

                    let mask_bits = dst.mask.0 & 0xF;
                    let load_lane = |bit: u8, offset: u32| {
                        if (mask_bits & bit) != 0 {
                            if used_uavs_atomic.contains(&uav.slot) {
                                format!("atomicLoad(&u{}.data[{base_name} + {offset}u])", uav.slot)
                            } else {
                                format!("u{}.data[{base_name} + {offset}u]", uav.slot)
                            }
                        } else {
                            "0u".to_owned()
                        }
                    };

                    let u_name = format!("ld_uav_raw_u{inst_index}");
                    w.line(&format!(
                        "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                        load_lane(1, 0),
                        load_lane(2, 1),
                        load_lane(4, 2),
                        load_lane(8, 3),
                    ));
                    let f_name = format!("ld_uav_raw_f{inst_index}");
                    w.line(&format!(
                        "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                    ));

                    let rhs = maybe_saturate(dst.saturate, f_name);
                    emit_write_masked(&mut w, inst_index, "ld_uav_raw", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::LdStructured {
                    dst,
                    index,
                    offset,
                    buffer,
                } => {
                    // Structured SRV loads read 1–4 consecutive `u32` words from a byte offset within
                    // the element.
                    let Some((kind, stride)) = srv_buffer_decls.get(&buffer.slot).copied() else {
                        return Err(GsTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "ld_structured",
                        });
                    };
                    if kind != BufferKind::Structured || stride == 0 || (stride % 4) != 0 {
                        return Err(GsTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "ld_structured",
                        });
                    }

                    let index_u32 =
                        emit_src_scalar_u32_addr(inst_index, "ld_structured", index, &input_sivs)?;
                    let offset_u32 =
                        emit_src_scalar_u32_addr(inst_index, "ld_structured", offset, &input_sivs)?;
                    // Keep index/offset in locals before multiplying by `stride`. Some address
                    // operands are constant immediates (often from float-literal bit patterns), and
                    // constant folding of overflowing `u32` multiplications can fail WGSL parsing.
                    let index_name = format!("ld_struct_index{inst_index}");
                    let offset_name = format!("ld_struct_offset{inst_index}");
                    w.line(&format!("var {index_name}: u32 = ({index_u32});"));
                    w.line(&format!("var {offset_name}: u32 = ({offset_u32});"));
                    let base_name = format!("ld_struct_base{inst_index}");
                    w.line(&format!(
                        "let {base_name}: u32 = (({index_name}) * {stride}u + ({offset_name})) / 4u;"
                    ));

                    let mask_bits = dst.mask.0 & 0xF;
                    let load_lane = |bit: u8, offset: u32| {
                        if (mask_bits & bit) != 0 {
                            format!("t{}.data[{base_name} + {offset}u]", buffer.slot)
                        } else {
                            "0u".to_owned()
                        }
                    };

                    let u_name = format!("ld_struct_u{inst_index}");
                    w.line(&format!(
                        "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                        load_lane(1, 0),
                        load_lane(2, 1),
                        load_lane(4, 2),
                        load_lane(8, 3),
                    ));
                    let f_name = format!("ld_struct_f{inst_index}");
                    w.line(&format!(
                        "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                    ));

                    let rhs = maybe_saturate(dst.saturate, f_name);
                    emit_write_masked(&mut w, inst_index, "ld_structured", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::LdStructuredUav {
                    dst,
                    index,
                    offset,
                    uav,
                } => {
                    let Some((kind, stride)) = uav_buffer_decls.get(&uav.slot).copied() else {
                        return Err(GsTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "ld_structured_uav",
                        });
                    };
                    if kind != BufferKind::Structured || stride == 0 || (stride % 4) != 0 {
                        return Err(GsTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "ld_structured_uav",
                        });
                    }

                    let index_u32 = emit_src_scalar_u32_addr(
                        inst_index,
                        "ld_structured_uav",
                        index,
                        &input_sivs,
                    )?;
                    let offset_u32 = emit_src_scalar_u32_addr(
                        inst_index,
                        "ld_structured_uav",
                        offset,
                        &input_sivs,
                    )?;
                    let index_name = format!("ld_uav_struct_index{inst_index}");
                    let offset_name = format!("ld_uav_struct_offset{inst_index}");
                    w.line(&format!("var {index_name}: u32 = ({index_u32});"));
                    w.line(&format!("var {offset_name}: u32 = ({offset_u32});"));
                    let base_name = format!("ld_uav_struct_base{inst_index}");
                    w.line(&format!(
                        "let {base_name}: u32 = (({index_name}) * {stride}u + ({offset_name})) / 4u;"
                    ));

                    let mask_bits = dst.mask.0 & 0xF;
                    let load_lane = |bit: u8, offset: u32| {
                        if (mask_bits & bit) != 0 {
                            if used_uavs_atomic.contains(&uav.slot) {
                                format!("atomicLoad(&u{}.data[{base_name} + {offset}u])", uav.slot)
                            } else {
                                format!("u{}.data[{base_name} + {offset}u]", uav.slot)
                            }
                        } else {
                            "0u".to_owned()
                        }
                    };

                    let u_name = format!("ld_uav_struct_u{inst_index}");
                    w.line(&format!(
                        "let {u_name}: vec4<u32> = vec4<u32>({}, {}, {}, {});",
                        load_lane(1, 0),
                        load_lane(2, 1),
                        load_lane(4, 2),
                        load_lane(8, 3),
                    ));
                    let f_name = format!("ld_uav_struct_f{inst_index}");
                    w.line(&format!(
                        "let {f_name}: vec4<f32> = bitcast<vec4<f32>>({u_name});"
                    ));

                    let rhs = maybe_saturate(dst.saturate, f_name);
                    emit_write_masked(
                        &mut w,
                        inst_index,
                        "ld_structured_uav",
                        dst.reg,
                        dst.mask,
                        rhs,
                    )?;
                }
                Sm4Inst::StoreRaw {
                    uav,
                    addr,
                    value,
                    mask,
                } => {
                    // Raw UAV stores use byte offsets.
                    let mask_bits = mask.0 & 0xF;
                    if mask_bits != 0 {
                        let addr_u32 =
                            emit_src_scalar_u32_addr(inst_index, "store_raw", addr, &input_sivs)?;
                        let base_name = format!("store_raw_base{inst_index}");
                        w.line(&format!("let {base_name}: u32 = ({addr_u32}) / 4u;"));

                        // Store raw bits. Buffer stores must preserve the underlying 32-bit lane
                        // patterns.
                        let value_u =
                            emit_src_vec4_u32(inst_index, "store_raw", value, &input_sivs)?;
                        let value_name = format!("store_raw_val{inst_index}");
                        w.line(&format!("let {value_name}: vec4<u32> = {value_u};"));

                        let comps = [
                            ('x', 1u8, 0u32),
                            ('y', 2u8, 1),
                            ('z', 4u8, 2),
                            ('w', 8u8, 3),
                        ];
                        for (c, bit, offset) in comps {
                            if (mask_bits & bit) != 0 {
                                if used_uavs_atomic.contains(&uav.slot) {
                                    w.line(&format!(
                                        "atomicStore(&u{}.data[{base_name} + {offset}u], ({value_name}).{c});",
                                        uav.slot
                                    ));
                                } else {
                                    w.line(&format!(
                                        "u{}.data[{base_name} + {offset}u] = ({value_name}).{c};",
                                        uav.slot
                                    ));
                                }
                            }
                        }
                    }
                }
                Sm4Inst::StoreStructured {
                    uav,
                    index,
                    offset,
                    value,
                    mask,
                } => {
                    let mask_bits = mask.0 & 0xF;
                    if mask_bits != 0 {
                        let Some((kind, stride)) = uav_buffer_decls.get(&uav.slot).copied() else {
                            return Err(GsTranslateError::UnsupportedInstruction {
                                inst_index,
                                opcode: "store_structured",
                            });
                        };
                        if kind != BufferKind::Structured || stride == 0 || (stride % 4) != 0 {
                            return Err(GsTranslateError::UnsupportedInstruction {
                                inst_index,
                                opcode: "store_structured",
                            });
                        }

                        let index_u32 = emit_src_scalar_u32_addr(
                            inst_index,
                            "store_structured",
                            index,
                            &input_sivs,
                        )?;
                        let offset_u32 = emit_src_scalar_u32_addr(
                            inst_index,
                            "store_structured",
                            offset,
                            &input_sivs,
                        )?;
                        let index_name = format!("store_struct_index{inst_index}");
                        let offset_name = format!("store_struct_offset{inst_index}");
                        w.line(&format!("var {index_name}: u32 = ({index_u32});"));
                        w.line(&format!("var {offset_name}: u32 = ({offset_u32});"));
                        let base_name = format!("store_struct_base{inst_index}");
                        w.line(&format!(
                            "let {base_name}: u32 = (({index_name}) * {stride}u + ({offset_name})) / 4u;"
                        ));

                        let value_u =
                            emit_src_vec4_u32(inst_index, "store_structured", value, &input_sivs)?;
                        let value_name = format!("store_struct_val{inst_index}");
                        w.line(&format!("let {value_name}: vec4<u32> = {value_u};"));

                        let comps = [
                            ('x', 1u8, 0u32),
                            ('y', 2u8, 1),
                            ('z', 4u8, 2),
                            ('w', 8u8, 3),
                        ];
                        for (c, bit, offset) in comps {
                            if (mask_bits & bit) != 0 {
                                if used_uavs_atomic.contains(&uav.slot) {
                                    w.line(&format!(
                                        "atomicStore(&u{}.data[{base_name} + {offset}u], ({value_name}).{c});",
                                        uav.slot
                                    ));
                                } else {
                                    w.line(&format!(
                                        "u{}.data[{base_name} + {offset}u] = ({value_name}).{c};",
                                        uav.slot
                                    ));
                                }
                            }
                        }
                    }
                }
                Sm4Inst::StoreUavTyped {
                    uav,
                    coord,
                    value,
                    mask,
                } => {
                    let Some(&format) = used_uav_textures.get(&uav.slot) else {
                        return Err(GsTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "store_uav_typed",
                        });
                    };

                    // DXBC `store_uav_typed` carries a write mask on the `u#` operand. WebGPU/WGSL
                    // `textureStore()` always writes a whole texel, so partial component stores
                    // would require a read-modify-write sequence (not supported yet).
                    //
                    // Many typed UAV formats have fewer than 4 channels (`r32*`, `rg32*`). For
                    // those, ignore writes to unused components and require that all meaningful
                    // components be present in the mask.
                    let required_mask = match format {
                        StorageTextureFormat::R32Float
                        | StorageTextureFormat::R32Uint
                        | StorageTextureFormat::R32Sint => WriteMask::X.0,
                        StorageTextureFormat::Rg32Float
                        | StorageTextureFormat::Rg32Uint
                        | StorageTextureFormat::Rg32Sint => WriteMask::X.0 | WriteMask::Y.0,
                        _ => WriteMask::XYZW.0,
                    };

                    let mask_bits = mask.0 & 0xF;
                    let effective_mask = mask_bits & required_mask;
                    if effective_mask != 0 && effective_mask != required_mask {
                        return Err(GsTranslateError::UnsupportedOperand {
                            inst_index,
                            opcode: "store_uav_typed",
                            msg: format!(
                                "partial typed UAV stores are not supported (format={format:?}, mask={mask:?})",
                            ),
                        });
                    }

                    // Typed UAV stores use integer texel coordinates, similar to `ld`.
                    //
                    // DXBC registers are untyped; interpret the coordinate lanes strictly as
                    // integer bits (bitcast `f32` -> `i32`) with no float-to-int heuristics.
                    let coord_i =
                        emit_src_vec4_i32(inst_index, "store_uav_typed", coord, &input_sivs)?;
                    let x = format!("({coord_i}).x");
                    let y = format!("({coord_i}).y");

                    let value = match uav_typed_value_type(format) {
                        UavTypedValueType::F32 => {
                            emit_src_vec4(inst_index, "store_uav_typed", value, &input_sivs)?
                        }
                        UavTypedValueType::U32 => {
                            emit_src_vec4_u32(inst_index, "store_uav_typed", value, &input_sivs)?
                        }
                        UavTypedValueType::I32 => {
                            emit_src_vec4_i32(inst_index, "store_uav_typed", value, &input_sivs)?
                        }
                    };

                    // Only emit the store when at least one meaningful component is enabled.
                    if effective_mask != 0 {
                        w.line(&format!(
                            "textureStore(u{}, vec2<i32>({x}, {y}), {value});",
                            uav.slot
                        ));
                    }
                }
                Sm4Inst::AtomicAdd {
                    dst,
                    uav,
                    addr,
                    value,
                } => {
                    let addr_u32 =
                        emit_src_scalar_u32(inst_index, "atomic_add", addr, &input_sivs)?;
                    let value_u32 =
                        emit_src_scalar_u32(inst_index, "atomic_add", value, &input_sivs)?;
                    let ptr = format!("&u{}.data[{addr_u32}]", uav.slot);

                    match dst {
                        Some(dst) => {
                            let tmp = format!("atomic_old_{inst_index}");
                            w.line(&format!("let {tmp}: u32 = atomicAdd({ptr}, {value_u32});"));
                            let rhs = format!("vec4<f32>(bitcast<f32>({tmp}))");
                            emit_write_masked(
                                &mut w,
                                inst_index,
                                "atomic_add",
                                dst.reg,
                                dst.mask,
                                rhs,
                            )?;
                        }
                        None => {
                            w.line(&format!("atomicAdd({ptr}, {value_u32});"));
                        }
                    }
                }
                Sm4Inst::BufInfoRaw { dst, buffer } => {
                    let dwords = format!("arrayLength(&t{}.data)", buffer.slot);
                    let bytes = format!("({dwords}) * 4u");
                    let rhs = format!("bitcast<vec4<f32>>(vec4<u32>(({bytes}), 0u, 0u, 0u))");
                    emit_write_masked(&mut w, inst_index, "bufinfo", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::BufInfoStructured {
                    dst,
                    buffer,
                    stride_bytes,
                } => {
                    if *stride_bytes == 0 {
                        return Err(GsTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "bufinfo_structured_stride_zero",
                        });
                    }
                    let dwords = format!("arrayLength(&t{}.data)", buffer.slot);
                    let byte_size = format!("({dwords}) * 4u");
                    let stride = format!("{}u", stride_bytes);
                    let elem_count = format!("({byte_size}) / ({stride})");
                    let rhs = format!(
                        "bitcast<vec4<f32>>(vec4<u32>(({elem_count}), ({stride}), 0u, 0u))"
                    );
                    emit_write_masked(&mut w, inst_index, "bufinfo", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::BufInfoRawUav { dst, uav } => {
                    let dwords = format!("arrayLength(&u{}.data)", uav.slot);
                    let bytes = format!("({dwords}) * 4u");
                    let rhs = format!("bitcast<vec4<f32>>(vec4<u32>(({bytes}), 0u, 0u, 0u))");
                    emit_write_masked(&mut w, inst_index, "bufinfo", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::BufInfoStructuredUav {
                    dst,
                    uav,
                    stride_bytes,
                } => {
                    if *stride_bytes == 0 {
                        return Err(GsTranslateError::UnsupportedInstruction {
                            inst_index,
                            opcode: "bufinfo_structured_uav_stride_zero",
                        });
                    }
                    let dwords = format!("arrayLength(&u{}.data)", uav.slot);
                    let byte_size = format!("({dwords}) * 4u");
                    let stride = format!("{}u", stride_bytes);
                    let elem_count = format!("({byte_size}) / ({stride})");
                    let rhs = format!(
                        "bitcast<vec4<f32>>(vec4<u32>(({elem_count}), ({stride}), 0u, 0u))"
                    );
                    emit_write_masked(&mut w, inst_index, "bufinfo", dst.reg, dst.mask, rhs)?;
                }
                Sm4Inst::Emit { stream: _ } => {
                    w.line(&format!("gs_emit({gs_emit_args}); // emit"));
                }
                Sm4Inst::Cut { stream: _ } => {
                    w.line("gs_cut(&strip_len); // cut");
                }
                Sm4Inst::EmitThenCut { stream: _ } => {
                    w.line(&format!("gs_emit({gs_emit_args}); // emitthen_cut"));
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
            Ok(())
        })();

        if !predicates.is_empty() {
            for _ in &predicates {
                w.dedent();
                w.line("}");
            }
        }

        r?;
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

    // Compute entry point: one invocation per input primitive per draw instance.
    w.line("@compute @workgroup_size(1)");
    w.line(&format!(
        "fn {entry_point}(@builtin(global_invocation_id) id: vec3<u32>) {{"
    ));
    w.indent();
    w.line("let prim_id: u32 = id.x;");
    w.line("let draw_instance_id: u32 = id.y;");
    w.line("if (prim_id >= params.primitive_count) { return; }");
    w.line("if (draw_instance_id >= params.instance_count) { return; }");
    w.line("");
    w.line("var overflow: bool = false;");
    w.line(
        "for (var gs_instance_id: u32 = 0u; gs_instance_id < GS_INSTANCE_COUNT; gs_instance_id = gs_instance_id + 1u) {",
    );
    w.indent();
    w.line("gs_exec_primitive(draw_instance_id, prim_id, gs_instance_id, &overflow);");
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
    w.line("let overflow: bool = atomicLoad(&out_state.counters.overflow) != 0u;");
    w.line("if (overflow) {");
    w.indent();
    w.line("out_state.out_indirect.index_count = 0u;");
    w.line("out_state.out_indirect.instance_count = 0u;");
    w.line("out_state.out_indirect.first_index = 0u;");
    w.line("out_state.out_indirect.base_vertex = 0;");
    w.line("out_state.out_indirect.first_instance = 0u;");
    w.dedent();
    w.line("} else {");
    w.indent();
    w.line("let out_index_count: u32 = atomicLoad(&out_state.counters.index_count);");
    w.line("out_state.out_indirect.index_count = out_index_count;");
    // The executor dispatches the GS prepass once per draw instance, so the expanded geometry
    // already includes all instances. Emit a non-instanced indirect draw.
    w.line("out_state.out_indirect.instance_count = 1u;");
    w.line("out_state.out_indirect.first_index = 0u;");
    w.line("out_state.out_indirect.base_vertex = 0;");
    w.line("out_state.out_indirect.first_instance = 0u;");
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
            output_topology_kind,
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

#[allow(clippy::too_many_arguments)]
fn emit_add_with_carry(
    w: &mut WgslWriter,
    opcode: &'static str,
    inst_index: usize,
    dst_sum: &crate::sm4_ir::DstOperand,
    dst_carry: &crate::sm4_ir::DstOperand,
    a: &crate::sm4_ir::SrcOperand,
    b: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<(), GsTranslateError> {
    let a_expr = emit_src_vec4_u32(inst_index, opcode, a, input_sivs)?;
    let b_expr = emit_src_vec4_u32(inst_index, opcode, b, input_sivs)?;

    // DXBC integer ops operate on raw 32-bit lanes. Model them as per-lane `u32` math and then
    // store the raw bits back into the untyped `vec4<f32>` register file.
    let a_var = format!("{opcode}_a_{inst_index}");
    let b_var = format!("{opcode}_b_{inst_index}");
    let sum_var = format!("{opcode}_sum_{inst_index}");
    let carry_var = format!("{opcode}_carry_{inst_index}");

    w.line(&format!("let {a_var} = {a_expr};"));
    w.line(&format!("let {b_var} = {b_expr};"));
    w.line(&format!("let {sum_var} = {a_var} + {b_var};"));
    w.line(&format!(
        "let {carry_var} = select(vec4<u32>(0u), vec4<u32>(1u), {sum_var} < {a_var});"
    ));

    let sum_bits = format!("bitcast<vec4<f32>>({sum_var})");
    emit_write_masked(w, inst_index, opcode, dst_sum.reg, dst_sum.mask, sum_bits)?;

    let carry_bits = format!("bitcast<vec4<f32>>({carry_var})");
    emit_write_masked(
        w,
        inst_index,
        opcode,
        dst_carry.reg,
        dst_carry.mask,
        carry_bits,
    )?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_sub_with_borrow(
    w: &mut WgslWriter,
    opcode: &'static str,
    inst_index: usize,
    dst_diff: &crate::sm4_ir::DstOperand,
    dst_borrow: &crate::sm4_ir::DstOperand,
    a: &crate::sm4_ir::SrcOperand,
    b: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<(), GsTranslateError> {
    let a_expr = emit_src_vec4_u32(inst_index, opcode, a, input_sivs)?;
    let b_expr = emit_src_vec4_u32(inst_index, opcode, b, input_sivs)?;

    let a_var = format!("{opcode}_a_{inst_index}");
    let b_var = format!("{opcode}_b_{inst_index}");
    let diff_var = format!("{opcode}_diff_{inst_index}");
    let borrow_var = format!("{opcode}_borrow_{inst_index}");

    w.line(&format!("let {a_var} = {a_expr};"));
    w.line(&format!("let {b_var} = {b_expr};"));
    w.line(&format!("let {diff_var} = {a_var} - {b_var};"));
    w.line(&format!(
        "let {borrow_var} = select(vec4<u32>(0u), vec4<u32>(1u), {a_var} < {b_var});"
    ));

    let diff_bits = format!("bitcast<vec4<f32>>({diff_var})");
    emit_write_masked(
        w,
        inst_index,
        opcode,
        dst_diff.reg,
        dst_diff.mask,
        diff_bits,
    )?;

    let borrow_bits = format!("bitcast<vec4<f32>>({borrow_var})");
    emit_write_masked(
        w,
        inst_index,
        opcode,
        dst_borrow.reg,
        dst_borrow.mask,
        borrow_bits,
    )?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_sub_with_carry(
    w: &mut WgslWriter,
    opcode: &'static str,
    inst_index: usize,
    dst_diff: &crate::sm4_ir::DstOperand,
    dst_carry: &crate::sm4_ir::DstOperand,
    a: &crate::sm4_ir::SrcOperand,
    b: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<(), GsTranslateError> {
    // Treat sources as raw 32-bit integer lanes in the untyped register file.
    let a_expr = emit_src_vec4_u32(inst_index, opcode, a, input_sivs)?;
    let b_expr = emit_src_vec4_u32(inst_index, opcode, b, input_sivs)?;

    let a_var = format!("{opcode}_a_{inst_index}");
    let b_var = format!("{opcode}_b_{inst_index}");
    let diff_var = format!("{opcode}_diff_{inst_index}");
    let carry_var = format!("{opcode}_carry_{inst_index}");

    w.line(&format!("let {a_var} = {a_expr};"));
    w.line(&format!("let {b_var} = {b_expr};"));
    w.line(&format!("let {diff_var} = {a_var} - {b_var};"));
    // Carry flag for subtraction is the inverse of borrow: 1 when no borrow occurred.
    w.line(&format!(
        "let {carry_var} = select(vec4<u32>(0u), vec4<u32>(1u), {a_var} >= {b_var});"
    ));

    let diff_bits = format!("bitcast<vec4<f32>>({diff_var})");
    emit_write_masked(
        w,
        inst_index,
        opcode,
        dst_diff.reg,
        dst_diff.mask,
        diff_bits,
    )?;

    let carry_bits = format!("bitcast<vec4<f32>>({carry_var})");
    emit_write_masked(
        w,
        inst_index,
        opcode,
        dst_carry.reg,
        dst_carry.mask,
        carry_bits,
    )?;

    Ok(())
}

fn emit_u32_mul_hi(a: &str, b: &str) -> String {
    let lanes =
        ['x', 'y', 'z', 'w'].map(|c| format!("u32((u64(({a}).{c}) * u64(({b}).{c})) >> 32u)"));
    format!(
        "vec4<u32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn emit_u32_mad_hi(a: &str, b: &str, c: &str) -> String {
    let lanes = ['x', 'y', 'z', 'w'].map(|lane| {
        format!("u32((u64(({a}).{lane}) * u64(({b}).{lane}) + u64(({c}).{lane})) >> 32u)")
    });
    format!(
        "vec4<u32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn emit_i32_mul_hi(a: &str, b: &str) -> String {
    let lanes =
        ['x', 'y', 'z', 'w'].map(|c| format!("i32((i64(({a}).{c}) * i64(({b}).{c})) >> 32u)"));
    format!(
        "vec4<i32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn emit_i32_mad_hi(a: &str, b: &str, c: &str) -> String {
    let lanes = ['x', 'y', 'z', 'w'].map(|lane| {
        format!("i32((i64(({a}).{lane}) * i64(({b}).{lane}) + i64(({c}).{lane})) >> 32u)")
    });
    format!(
        "vec4<i32>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn bump_reg_max(reg: RegisterRef, max_temp_reg: &mut i32, max_output_reg: &mut i32) {
    match reg.file {
        RegFile::Temp => *max_temp_reg = (*max_temp_reg).max(reg.index as i32),
        RegFile::Output => *max_output_reg = (*max_output_reg).max(reg.index as i32),
        _ => {}
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
    cbuffer_decls: &BTreeMap<u32, u32>,
    used_cbuffers: &mut BTreeMap<u32, u32>,
) -> Result<(), GsTranslateError> {
    match &src.kind {
        SrcKind::Register(reg) => {
            bump_reg_max(*reg, max_temp_reg, max_output_reg);
            match reg.file {
                RegFile::Temp | RegFile::Output => {}
                RegFile::Null => {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: "RegFile::Null is write-only and cannot be used as a source operand"
                            .to_owned(),
                    });
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
                RegFile::OutputDepth => {
                    return Err(GsTranslateError::UnsupportedOperand {
                        inst_index,
                        opcode,
                        msg: "RegFile::OutputDepth is not supported in GS prepass".to_owned(),
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
        SrcKind::ConstantBuffer { slot, reg } => {
            let Some(&reg_count) = cbuffer_decls.get(slot) else {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: format!(
                        "constant buffer cb{slot}[{reg}] used but slot cb{slot} is not declared via dcl_constantbuffer"
                    ),
                });
            };
            if *reg >= reg_count {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: format!(
                        "constant buffer cb{slot}[{reg}] is out of bounds (declared reg_count={reg_count})"
                    ),
                });
            }
            used_cbuffers.entry(*slot).or_insert(reg_count);
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
        RegFile::Null => {
            // Discarded result (DXBC `null` destination).
            return Ok(());
        }
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

fn emit_write_masked_bool(
    w: &mut WgslWriter,
    dst: PredicateDstOperand,
    rhs: String,
) -> Result<(), GsTranslateError> {
    let dst_expr = format!("p{}", dst.reg.index);

    // Mask is 4 bits.
    let mask_bits = dst.mask.0 & 0xF;
    if mask_bits == 0 {
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

fn emit_sm4_cmp_op_scalar_bool(op: Sm4CmpOp, a: &str, b: &str) -> String {
    match op {
        // Ordered comparisons.
        Sm4CmpOp::Eq => format!("({a}) == ({b})"),
        // NOTE: ordered "not equal" is false when either operand is NaN.
        // WGSL doesn't expose a NaN test in the standard library (in the naga/WGSL version we
        // target), so use the IEEE property that `NaN != NaN`.
        Sm4CmpOp::Ne => format!("((({a}) != ({b})) && (({a}) == ({a})) && (({b}) == ({b})))"),
        Sm4CmpOp::Lt => format!("({a}) < ({b})"),
        Sm4CmpOp::Ge => format!("({a}) >= ({b})"),
        Sm4CmpOp::Le => format!("({a}) <= ({b})"),
        Sm4CmpOp::Gt => format!("({a}) > ({b})"),
        // Unordered comparisons (`*_U`) are true if either operand is NaN.
        //
        // Use `x != x` to test for NaN (true iff NaN).
        Sm4CmpOp::EqU => format!("((({a}) == ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::NeU => format!("((({a}) != ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::LtU => format!("((({a}) < ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::GeU => format!("((({a}) >= ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::LeU => format!("((({a}) <= ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
        Sm4CmpOp::GtU => format!("((({a}) > ({b})) || (({a}) != ({a})) || (({b}) != ({b})))"),
    }
}

fn emit_sm4_cmp_op_vec4_bool(op: Sm4CmpOp, a_vec4: &str, b_vec4: &str) -> String {
    let comps = ['x', 'y', 'z', 'w'];
    let mut lanes = Vec::with_capacity(4);
    for c in comps {
        let a = format!("({a_vec4}).{c}");
        let b = format!("({b_vec4}).{c}");
        lanes.push(emit_sm4_cmp_op_scalar_bool(op, &a, &b));
    }
    format!(
        "vec4<bool>({}, {}, {}, {})",
        lanes[0], lanes[1], lanes[2], lanes[3]
    )
}

fn emit_test_predicate_scalar(pred: &PredicateOperand) -> String {
    let base = format!("p{}.{}", pred.reg.index, component_char(pred.component));
    if pred.invert {
        format!("!({base})")
    } else {
        base
    }
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
            RegFile::Null => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: "RegFile::Null is write-only and cannot be used as a source operand"
                        .to_owned(),
                })
            }
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
        SrcKind::GsInput { reg, vertex } => {
            format!("gs_load_input(draw_instance_id, prim_id, {reg}u, {vertex}u)")
        }
        SrcKind::ConstantBuffer { slot, reg } => {
            format!("bitcast<vec4<f32>>(cb{slot}.regs[{reg}])")
        }
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
            RegFile::Null => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: "RegFile::Null is write-only and cannot be used as a source operand"
                        .to_owned(),
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
        SrcKind::GsInput { reg, vertex } => format!(
            "bitcast<vec4<u32>>(gs_load_input(draw_instance_id, prim_id, {reg}u, {vertex}u))"
        ),
        SrcKind::ConstantBuffer { slot, reg } => format!("cb{slot}.regs[{reg}]"),
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

fn emit_src_scalar_u32_addr(
    inst_index: usize,
    opcode: &'static str,
    src: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<String, GsTranslateError> {
    // Match `shader_translate.rs`: address operands are consumed as raw integer bits; any float→int
    // numeric conversion must be expressed explicitly in DXBC.
    emit_src_scalar_u32(inst_index, opcode, src, input_sivs)
}

fn emit_src_scalar_u32(
    inst_index: usize,
    opcode: &'static str,
    src: &crate::sm4_ir::SrcOperand,
    input_sivs: &HashMap<u32, InputSivInfo>,
) -> Result<String, GsTranslateError> {
    let u = emit_src_vec4_u32(inst_index, opcode, src, input_sivs)?;
    Ok(format!("({u}).x"))
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
            RegFile::Null => {
                return Err(GsTranslateError::UnsupportedOperand {
                    inst_index,
                    opcode,
                    msg: "RegFile::Null is write-only and cannot be used as a source operand"
                        .to_owned(),
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
        SrcKind::GsInput { reg, vertex } => format!(
            "bitcast<vec4<i32>>(gs_load_input(draw_instance_id, prim_id, {reg}u, {vertex}u))"
        ),
        SrcKind::ConstantBuffer { slot, reg } => {
            format!("bitcast<vec4<i32>>(cb{slot}.regs[{reg}])")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UavTypedValueType {
    F32,
    U32,
    I32,
}

fn decode_uav_typed2d_format_dxgi(dxgi_format: u32) -> Option<StorageTextureFormat> {
    Some(match dxgi_format {
        // DXGI_FORMAT_R8G8B8A8_UNORM
        28 => StorageTextureFormat::Rgba8Unorm,
        // DXGI_FORMAT_R8G8B8A8_SNORM
        31 => StorageTextureFormat::Rgba8Snorm,
        // DXGI_FORMAT_R8G8B8A8_UINT
        30 => StorageTextureFormat::Rgba8Uint,
        // DXGI_FORMAT_R8G8B8A8_SINT
        32 => StorageTextureFormat::Rgba8Sint,
        // DXGI_FORMAT_R16G16B16A16_FLOAT
        10 => StorageTextureFormat::Rgba16Float,
        // DXGI_FORMAT_R16G16B16A16_UINT
        12 => StorageTextureFormat::Rgba16Uint,
        // DXGI_FORMAT_R16G16B16A16_SINT
        14 => StorageTextureFormat::Rgba16Sint,
        // DXGI_FORMAT_R32G32_FLOAT
        16 => StorageTextureFormat::Rg32Float,
        // DXGI_FORMAT_R32G32_UINT
        17 => StorageTextureFormat::Rg32Uint,
        // DXGI_FORMAT_R32G32_SINT
        18 => StorageTextureFormat::Rg32Sint,
        // DXGI_FORMAT_R32G32B32A32_FLOAT
        2 => StorageTextureFormat::Rgba32Float,
        // DXGI_FORMAT_R32G32B32A32_UINT
        3 => StorageTextureFormat::Rgba32Uint,
        // DXGI_FORMAT_R32G32B32A32_SINT
        4 => StorageTextureFormat::Rgba32Sint,
        // DXGI_FORMAT_R32_FLOAT
        41 => StorageTextureFormat::R32Float,
        // DXGI_FORMAT_R32_UINT
        42 => StorageTextureFormat::R32Uint,
        // DXGI_FORMAT_R32_SINT
        43 => StorageTextureFormat::R32Sint,
        _ => return None,
    })
}

fn uav_typed_value_type(format: StorageTextureFormat) -> UavTypedValueType {
    match format {
        StorageTextureFormat::Rgba8Unorm
        | StorageTextureFormat::Rgba8Snorm
        | StorageTextureFormat::Rgba16Float
        | StorageTextureFormat::Rg32Float
        | StorageTextureFormat::Rgba32Float
        | StorageTextureFormat::R32Float => UavTypedValueType::F32,
        StorageTextureFormat::Rgba8Uint
        | StorageTextureFormat::Rgba16Uint
        | StorageTextureFormat::Rg32Uint
        | StorageTextureFormat::Rgba32Uint
        | StorageTextureFormat::R32Uint => UavTypedValueType::U32,
        StorageTextureFormat::Rgba8Sint
        | StorageTextureFormat::Rgba16Sint
        | StorageTextureFormat::Rg32Sint
        | StorageTextureFormat::Rgba32Sint
        | StorageTextureFormat::R32Sint => UavTypedValueType::I32,
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
            wgsl.contains("const GS_INSTANCE_COUNT: u32 = 2u;"),
            "expected gs instance count constant in WGSL:\n{wgsl}"
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
    fn gs_translate_disambiguates_output_topology_token_3() {
        // `dcl_outputtopology` token value 3 is ambiguous:
        // - Tokenized shader format: triangle strip = 3
        // - D3D primitive topology enum: line strip = 3
        //
        // Disambiguate based on the input primitive encoding: when the input primitive looks like
        // it used D3D topology constants (e.g. triangle list = 4), treat output value 3 as line
        // strip instead of triangle strip.
        let tokenized = Sm4Module {
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
            instructions: vec![Sm4Inst::Ret],
        };
        assert_eq!(
            module_output_topology_kind(&tokenized).expect("decode output topology"),
            GsOutputTopologyKind::TriangleStrip
        );

        let d3d_encoded = Sm4Module {
            stage: ShaderStage::Geometry,
            model: ShaderModel { major: 4, minor: 0 },
            decls: vec![
                // D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST = 4 (strong signal that the toolchain is
                // using D3D topology constants for GS decls).
                Sm4Decl::GsInputPrimitive {
                    primitive: GsInputPrimitive::Triangle(4),
                },
                Sm4Decl::GsOutputTopology {
                    topology: GsOutputTopology::TriangleStrip(3),
                },
                Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            ],
            instructions: vec![Sm4Inst::Ret],
        };
        assert_eq!(
            module_output_topology_kind(&d3d_encoded).expect("decode output topology"),
            GsOutputTopologyKind::LineStrip
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
            wgsl.contains("gs_exec_primitive(draw_instance_id, prim_id, gs_instance_id,"),
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
    fn gs_translate_fixed_array_layout_matches_passthrough_vs_expanded_vertex_format() {
        // The cmd-stream executor's passthrough VS (`runtime/wgsl_link.rs`) expects the expanded
        // vertex record to be:
        //   - pos: vec4<f32>
        //   - varyings: array<vec4<f32>, 32>
        //
        // Ensure the internal fixed-array GS translation mode emits that layout and maps `oN`
        // outputs into `varyings[N]`.
        let module = Sm4Module {
            stage: ShaderStage::Geometry,
            model: ShaderModel { major: 4, minor: 0 },
            decls: vec![
                Sm4Decl::GsInputPrimitive {
                    primitive: GsInputPrimitive::Point(1),
                },
                Sm4Decl::GsOutputTopology {
                    topology: GsOutputTopology::Point(1),
                },
                Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            ],
            instructions: vec![
                // Initialize outputs required by `emit`.
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Emit { stream: 0 },
                Sm4Inst::Ret,
            ],
        };

        let wgsl =
            translate_gs_module_to_wgsl_compute_prepass_with_entry_point_fixed(&module, "cs_main")
                .expect("translation should succeed")
                .wgsl;

        assert!(
            wgsl.contains("varyings: array<vec4<f32>, 32>"),
            "expected fixed ExpandedVertex varyings array layout in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].varyings = array<vec4<f32>, 32>();"),
            "expected gs_emit to zero-initialize the varyings array in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].varyings[1u] = o1;"),
            "expected o1 to map to varyings[1] in expanded vertex output:\n{wgsl}"
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

    #[test]
    fn gs_translate_emits_packed_expanded_vertex_fields() {
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
            instructions: vec![Sm4Inst::Ret],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass_packed(&module, &[1, 7])
            .expect("translation should succeed");
        assert!(
            wgsl.contains("v0: vec4<f32>,") && wgsl.contains("v1: vec4<f32>,"),
            "expected packed ExpandedVertex fields v0/v1 in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].v0 = o1;"),
            "expected varying location 1 to map to ExpandedVertex.v0:\n{wgsl}"
        );
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].v1 = o7;"),
            "expected varying location 7 to map to ExpandedVertex.v1:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_fixed_emits_expanded_vertex_varying_table() {
        // Regression test: the fixed-layout GS prepass translator must write into the expanded
        // vertex varying table consumed by the autogenerated passthrough VS (`runtime::wgsl_link`).
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
                // Initialize a couple of output registers so they are referenced by the shader.
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 7,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Emit { stream: 0 },
                Sm4Inst::Ret,
            ],
        };

        let wgsl =
            translate_gs_module_to_wgsl_compute_prepass_with_entry_point_fixed(&module, "cs_main")
                .expect("translation should succeed")
                .wgsl;
        assert!(
            wgsl.contains(&format!(
                "varyings: array<vec4<f32>, {EXPANDED_VERTEX_MAX_VARYINGS}>,"
            )),
            "expected fixed expanded-vertex varying table in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].varyings[7u] = o7;"),
            "expected varying location 7 store:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_defaults_to_declared_output_varyings() {
        // The convenience `translate_gs_module_to_wgsl_compute_prepass` helper should export all
        // declared GS outputs (excluding `o0` position) into the expanded-vertex record.
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
                Sm4Decl::Output {
                    reg: 0,
                    mask: WriteMask::XYZW,
                },
                Sm4Decl::Output {
                    reg: 1,
                    mask: WriteMask::XYZW,
                },
                Sm4Decl::Output {
                    reg: 7,
                    mask: WriteMask::XYZW,
                },
            ],
            instructions: vec![Sm4Inst::Ret],
        };
        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].v0 = o1;"),
            "expected declared output o1 to be exported by default:\n{wgsl}"
        );
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].v1 = o7;"),
            "expected declared output o7 to be exported by default:\n{wgsl}"
        );
        assert!(
            !wgsl.contains("out_vertices.data[vtx_idx].v0 = o0;"),
            "expected location 0 to remain reserved for position:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_defaults_to_output_regs_when_output_decls_missing() {
        // Some SM4 token streams omit explicit `dcl_output` declarations even though the shader
        // writes to output registers. The convenience helper should still export those outputs as
        // varyings so the render pass sees the correct values.
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
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([0u32; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Emit { stream: 0 },
                Sm4Inst::Ret,
            ],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].v0 = o1;"),
            "expected output register o1 to be exported as varying when output decls are missing:\n{wgsl}"
        );
        assert!(
            !wgsl.contains("out_vertices.data[vtx_idx].v0 = o0;"),
            "expected location 0 to remain reserved for position:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_defaults_to_written_output_varyings_when_dcl_output_missing() {
        // Some real-world geometry shaders omit `dcl_output` declarations and instead rely on the
        // DXBC output signature (`OSGN`) to define their stage interface. Our `Sm4Module` IR does
        // not currently carry signature information, so ensure the default varyings selection falls
        // back to scanning instruction destinations for `o#` writes.
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
                // o0 = position
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([
                            0.0f32.to_bits(),
                            0.0f32.to_bits(),
                            0.0f32.to_bits(),
                            1.0f32.to_bits(),
                        ]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                // o1 = color
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([
                            1.0f32.to_bits(),
                            0.0f32.to_bits(),
                            0.0f32.to_bits(),
                            1.0f32.to_bits(),
                        ]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                // o7 = another varying
                Sm4Inst::Mov {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 7,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: SrcOperand {
                        kind: SrcKind::ImmediateF32([
                            0.0f32.to_bits(),
                            1.0f32.to_bits(),
                            0.0f32.to_bits(),
                            1.0f32.to_bits(),
                        ]),
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
            wgsl.contains("out_vertices.data[vtx_idx].v0 = o1;"),
            "expected output o1 to be exported by default even without a dcl_output declaration:\n{wgsl}"
        );
        assert!(
            wgsl.contains("out_vertices.data[vtx_idx].v1 = o7;"),
            "expected output o7 to be exported by default even without a dcl_output declaration:\n{wgsl}"
        );
        assert!(
            !wgsl.contains("out_vertices.data[vtx_idx].v0 = o0;"),
            "expected location 0 to remain reserved for position:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_emits_prepass_state_with_indirect_args_at_offset_0() {
        // Regression test: the executor feeds the GS prepass state buffer directly into
        // `draw_indexed_indirect`, so `DrawIndexedIndirectArgs` must be the first field in the
        // storage struct.
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
            instructions: vec![Sm4Inst::Ret],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");

        let struct_pos = wgsl
            .find("struct GsPrepassState {")
            .expect("expected GsPrepassState struct");
        let indirect_pos = wgsl[struct_pos..]
            .find("indirect: DrawIndexedIndirectArgs,")
            .map(|p| p + struct_pos)
            .expect("expected indirect field");
        let counters_pos = wgsl[struct_pos..]
            .find("counters: GsPrepassCounters,")
            .map(|p| p + struct_pos)
            .expect("expected counters field");
        assert!(
            indirect_pos < counters_pos,
            "expected DrawIndexedIndirectArgs (indirect) to appear before counters in GsPrepassState:\n{wgsl}"
        );
        assert!(
            wgsl.contains(
                "@group(0) @binding(2) var<storage, read_write> out_state: GsPrepassState;"
            ),
            "expected out_state binding at @group(0) @binding(2):\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_group0_storage_buffers_stay_within_downlevel_limit() {
        // Regression test: some downlevel backends report
        // `max_storage_buffers_per_shader_stage = 4`. Keep translated GS prepasses within that
        // limit by ensuring we do not accidentally re-introduce a 5th group(0) storage buffer
        // binding (e.g. by splitting counters back out of the indirect args buffer).
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
            instructions: vec![Sm4Inst::Ret],
        };

        let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module)
            .expect("translation should succeed");

        // Ensure the expected bindings exist.
        for expected in [
            "@group(0) @binding(0) var<storage, read_write> out_vertices:",
            "@group(0) @binding(1) var<storage, read_write> out_indices:",
            "@group(0) @binding(2) var<storage, read_write> out_state:",
            "@group(0) @binding(5) var<storage,",
            "gs_inputs:",
        ] {
            assert!(
                wgsl.contains(expected),
                "expected group(0) storage binding:\n  {expected}\nwgsl:\n{wgsl}"
            );
        }
        assert!(
            !wgsl.contains("var<storage, read_write> counters:"),
            "counters must not be a separate storage buffer binding in group(0):\n{wgsl}"
        );

        let group0_storage_bindings = wgsl
            .lines()
            .filter(|line| line.contains("@group(0)") && line.contains("var<storage"))
            .count();
        assert!(
            group0_storage_bindings <= 4,
            "expected <= 4 group(0) storage buffers for downlevel compatibility, got {group0_storage_bindings}:\n{wgsl}"
        );
    }

    #[test]
    fn gs_translate_rejects_invalid_varying_locations() {
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
            instructions: vec![Sm4Inst::Ret],
        };

        assert!(
            matches!(
                translate_gs_module_to_wgsl_compute_prepass_packed(&module, &[0]),
                Err(GsTranslateError::InvalidVaryingLocation { loc: 0 })
            ),
            "expected location 0 to be rejected"
        );
        assert!(
            matches!(
                translate_gs_module_to_wgsl_compute_prepass_packed(
                    &module,
                    &[EXPANDED_VERTEX_MAX_VARYINGS]
                ),
                Err(GsTranslateError::InvalidVaryingLocation { .. })
            ),
            "expected out-of-range varying location to be rejected"
        );
    }
}
