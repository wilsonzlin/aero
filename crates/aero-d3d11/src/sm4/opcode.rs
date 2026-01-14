//! SM4/SM5 opcode and operand numeric constants.
//!
//! The DXBC token stream encodes opcodes and operand kinds as numeric IDs in the
//! low bits of each token. We only define the subset needed by the current
//! translation pipeline (FL10_0 VS/PS).

/// Low 11 bits of an opcode token.
pub const OPCODE_MASK: u32 = 0x7ff;

/// Instruction length field (in DWORDs, including the opcode token).
pub const OPCODE_LEN_SHIFT: u32 = 11;
pub const OPCODE_LEN_MASK: u32 = 0x1fff;

/// If set on an opcode token, one or more extended opcode tokens follow.
pub const OPCODE_EXTENDED_BIT: u32 = 0x8000_0000;

// ---- Opcodes (subset) ----

pub const OPCODE_NOP: u32 = 0x00;
pub const OPCODE_MOV: u32 = 0x01;
/// `movc dst, cond, a, b` (conditional select).
pub const OPCODE_MOVC: u32 = 0x1c;
pub const OPCODE_ADD: u32 = 0x02;
pub const OPCODE_MAD: u32 = 0x04;
pub const OPCODE_MUL: u32 = 0x05;
pub const OPCODE_RCP: u32 = 0x06;
pub const OPCODE_RSQ: u32 = 0x07;
pub const OPCODE_DP3: u32 = 0x08;
pub const OPCODE_DP4: u32 = 0x09;
pub const OPCODE_MIN: u32 = 0x0a;
pub const OPCODE_MAX: u32 = 0x0b;

/// Unsigned integer add with carry: `uaddc dst_sum, dst_carry, a, b`.
pub const OPCODE_UADDC: u32 = 0x6a;
/// Unsigned integer subtract with borrow: `usubb dst_diff, dst_borrow, a, b`.
pub const OPCODE_USUBB: u32 = 0x6b;
/// Signed integer add with carry: `iaddc dst_sum, dst_carry, a, b`.
pub const OPCODE_IADDC: u32 = 0x6c;
/// Signed integer subtract with borrow/carry: `isubc dst_diff, dst_borrow, a, b`.
pub const OPCODE_ISUBC: u32 = 0x6d;

/// `udiv dst_quot, dst_rem, a, b` (unsigned integer quotient + remainder).
pub const OPCODE_UDIV: u32 = 0x3c;
/// `idiv dst_quot, dst_rem, a, b` (signed integer quotient + remainder).
pub const OPCODE_IDIV: u32 = 0x3d;

// ---- Control flow (structured) ----
//
// Canonical opcode IDs from `d3d10tokenizedprogramformat.hpp` / `d3d11tokenizedprogramformat.hpp`.
pub const OPCODE_IF: u32 = 0x28;
pub const OPCODE_ELSE: u32 = 0x29;
pub const OPCODE_ENDIF: u32 = 0x2a;

// ---- Integer arithmetic ----
pub const OPCODE_IABS: u32 = 0x61;
pub const OPCODE_INEG: u32 = 0x62;
pub const OPCODE_IMIN: u32 = 0x63;
pub const OPCODE_IMAX: u32 = 0x64;
pub const OPCODE_UMIN: u32 = 0x65;
pub const OPCODE_UMAX: u32 = 0x66;

/// `bfi` (bitfield insert).
pub const OPCODE_BFI: u32 = 0x67;
/// `ubfe` (unsigned bitfield extract).
pub const OPCODE_UBFE: u32 = 0x68;
/// `ibfe` (signed bitfield extract).
pub const OPCODE_IBFE: u32 = 0x69;

// ---- Integer comparison opcodes (SM4/SM5) ----
//
// These produce a per-component predicate mask: 0xffffffff for true, 0x00000000 for false.
// The numeric values match `D3D10_SB_OPCODE_*` from `d3d11tokenizedprogramformat.h`.
pub const OPCODE_IEQ: u32 = 0x20;
pub const OPCODE_IGE: u32 = 0x21;
pub const OPCODE_ILT: u32 = 0x22;
pub const OPCODE_INE: u32 = 0x27;
pub const OPCODE_ULT: u32 = 0x4f;
pub const OPCODE_UGE: u32 = 0x50;

/// Non-executable custom data / comment block.
///
/// Used for embedded comments, debug data, and immediate constant buffers.
pub const OPCODE_CUSTOMDATA: u32 = 0x1f;

// ---- Custom-data classes (`D3D10_SB_CUSTOMDATA_CLASS`) ----
//
// The class DWORD follows the `customdata` opcode token (after any extended opcode tokens).
pub const CUSTOMDATA_CLASS_COMMENT: u32 = 0;
/// Embedded immediate constant buffer data (`dcl_immediateConstantBuffer { ... }`).
pub const CUSTOMDATA_CLASS_IMMEDIATE_CONSTANT_BUFFER: u32 = 3;

// ---- Structured control flow ----

/// `break` (structured break out of `loop`/`switch`).
pub const OPCODE_BREAK: u32 = 0x2d;
/// `switch` (structured switch statement).
pub const OPCODE_SWITCH: u32 = 0x35;
/// `case` (case label within a `switch`).
pub const OPCODE_CASE: u32 = 0x36;
/// `default` (default label within a `switch`).
pub const OPCODE_DEFAULT: u32 = 0x37;
/// `endswitch` (end of structured `switch` body).
pub const OPCODE_ENDSWITCH: u32 = 0x38;

pub const OPCODE_RET: u32 = 0x3e;

// Geometry shader stream emission / cutting.
//
// Values from the D3D10+ tokenized shader format opcode table:
// `D3D10_SB_OPCODE_TYPE_EMIT`, `D3D10_SB_OPCODE_TYPE_CUT`,
// `D3D10_SB_OPCODE_TYPE_EMIT_STREAM`, `D3D10_SB_OPCODE_TYPE_CUT_STREAM`
// in the Windows SDK header `d3d10tokenizedprogramformat.h`.
pub const OPCODE_EMIT: u32 = 0x43;
pub const OPCODE_CUT: u32 = 0x44;
pub const OPCODE_EMIT_STREAM: u32 = 0x41;
pub const OPCODE_CUT_STREAM: u32 = 0x42;

pub const OPCODE_SAMPLE: u32 = 0x45;
pub const OPCODE_SAMPLE_L: u32 = 0x46;
/// `ld` (Resource load; used by `Texture2D.Load`).
pub const OPCODE_LD: u32 = 0x4c;
/// `ld_raw` (raw buffer load; `ByteAddressBuffer.Load*`).
///
/// Upstream: `D3D11_SB_OPCODE_LD_RAW`.
pub const OPCODE_LD_RAW: u32 = 0x53;
/// `ld_structured` (structured buffer load; `StructuredBuffer.Load`).
///
/// Upstream: `D3D11_SB_OPCODE_LD_STRUCTURED`.
pub const OPCODE_LD_STRUCTURED: u32 = 0x54;
/// `store_raw` (raw buffer store; `RWByteAddressBuffer.Store*`).
///
/// Upstream: `D3D11_SB_OPCODE_STORE_RAW`.
pub const OPCODE_STORE_RAW: u32 = 0x56;
/// `store_structured` (structured buffer store; `RWStructuredBuffer.Store`).
///
/// Upstream: `D3D11_SB_OPCODE_STORE_STRUCTURED`.
pub const OPCODE_STORE_STRUCTURED: u32 = 0x57;

/// `bfrev` (bit reverse).
///
/// Upstream: `D3D11_SB_OPCODE_BFREV`.
pub const OPCODE_BFREV: u32 = 0x58;
/// `countbits` (population count).
///
/// Upstream: `D3D11_SB_OPCODE_COUNTBITS`.
pub const OPCODE_COUNTBITS: u32 = 0x59;
/// `firstbit_hi` (find MSB set, unsigned).
///
/// Upstream: `D3D11_SB_OPCODE_FIRSTBIT_HI`.
pub const OPCODE_FIRSTBIT_HI: u32 = 0x5a;
/// `firstbit_lo` (find LSB set, unsigned).
///
/// Upstream: `D3D11_SB_OPCODE_FIRSTBIT_LO`.
pub const OPCODE_FIRSTBIT_LO: u32 = 0x5b;
/// `firstbit_shi` (find MSB differing from sign bit, signed).
///
/// Upstream: `D3D11_SB_OPCODE_FIRSTBIT_SHI`.
pub const OPCODE_FIRSTBIT_SHI: u32 = 0x5c;

/// `sync` (SM5 barrier / thread-group synchronization).
///
/// In DXBC the `sync` instruction encodes a set of barrier flags in the opcode token's
/// "opcode-specific control" field (bits 24..=30). This is used to represent HLSL intrinsics like:
/// - `GroupMemoryBarrierWithGroupSync()`
/// - `DeviceMemoryBarrierWithGroupSync()`
/// - `AllMemoryBarrierWithGroupSync()`
pub const OPCODE_SYNC: u32 = 0x5f;

/// Opcode token "opcode-specific control" field (bits 24..=30).
pub const OPCODE_CONTROL_SHIFT: u32 = 24;
pub const OPCODE_CONTROL_MASK: u32 = 0x7f;

// `sync` flag bits (subset of `D3D11_SB_SYNC_FLAGS`).
pub const SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY: u32 = 0x1;
pub const SYNC_FLAG_UAV_MEMORY: u32 = 0x2;
/// If set, the instruction is a full workgroup barrier (all threads must participate).
pub const SYNC_FLAG_THREAD_GROUP_SYNC: u32 = 0x4;

// ---- Declaration opcodes (subset) ----
//
// Values are sourced from the D3D10/11 tokenized shader format opcode table in the
// Windows SDK headers `d3d10tokenizedprogramformat.h` / `d3d11tokenizedprogramformat.h`.

/// `dcl_inputprimitive` (geometry shader input primitive).
pub const OPCODE_DCL_GS_INPUT_PRIMITIVE: u32 = 0x10c;
/// `dcl_outputtopology` (geometry shader output topology).
pub const OPCODE_DCL_GS_OUTPUT_TOPOLOGY: u32 = 0x10d;
/// `dcl_maxout` / `dcl_maxvertexcount` (geometry shader max output vertex count).
pub const OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT: u32 = 0x10e;
/// `dcl_gsinstancecount` / geometry shader instance count (SM5).
///
/// Upstream: `D3D11_SB_OPCODE_DCL_GS_INSTANCE_COUNT`.
pub const OPCODE_DCL_GS_INSTANCE_COUNT: u32 = 0x10f;

/// `dcl_thread_group` declaration.
///
/// Upstream: `D3D11_SB_OPCODE_DCL_THREAD_GROUP`.
pub const OPCODE_DCL_THREAD_GROUP: u32 = 0x11f;

/// `dcl_resource_raw t#` (raw SRV buffer; `ByteAddressBuffer`).
///
/// Upstream: `D3D11_SB_OPCODE_DCL_RESOURCE_RAW`.
pub const OPCODE_DCL_RESOURCE_RAW: u32 = 0x205;
/// `dcl_resource_structured t#, stride` (structured SRV buffer; `StructuredBuffer`).
///
/// Upstream: `D3D11_SB_OPCODE_DCL_RESOURCE_STRUCTURED`.
pub const OPCODE_DCL_RESOURCE_STRUCTURED: u32 = 0x206;
/// `dcl_uav_raw u#` (raw UAV buffer; `RWByteAddressBuffer`).
///
/// Upstream: `D3D11_SB_OPCODE_DCL_UAV_RAW`.
pub const OPCODE_DCL_UAV_RAW: u32 = 0x207;
/// `dcl_uav_structured u#, stride` (structured UAV buffer; `RWStructuredBuffer`).
///
/// Upstream: `D3D11_SB_OPCODE_DCL_UAV_STRUCTURED`.
pub const OPCODE_DCL_UAV_STRUCTURED: u32 = 0x208;

// ---- Opcode token bitfields ----
//
// Certain control-flow opcodes (e.g. `if`) encode a "test boolean" (zero vs non-zero) in the
// opcode token itself rather than using distinct opcode IDs for `if_z`/`if_nz`.
pub const OPCODE_TEST_BOOLEAN_SHIFT: u32 = 24;
pub const OPCODE_TEST_BOOLEAN_MASK: u32 = 0x3;

// ---- Operand token bitfields ----

pub const OPERAND_NUM_COMPONENTS_MASK: u32 = 0x3;
pub const OPERAND_SELECTION_MODE_SHIFT: u32 = 2;
pub const OPERAND_SELECTION_MODE_MASK: u32 = 0x3;
pub const OPERAND_TYPE_SHIFT: u32 = 4;
pub const OPERAND_TYPE_MASK: u32 = 0xff;
pub const OPERAND_COMPONENT_SELECTION_SHIFT: u32 = 12;
pub const OPERAND_COMPONENT_SELECTION_MASK: u32 = 0xff;
pub const OPERAND_INDEX_DIMENSION_SHIFT: u32 = 20;
pub const OPERAND_INDEX_DIMENSION_MASK: u32 = 0x3;
pub const OPERAND_INDEX0_REP_SHIFT: u32 = 22;
pub const OPERAND_INDEX1_REP_SHIFT: u32 = 25;
pub const OPERAND_INDEX2_REP_SHIFT: u32 = 28;
pub const OPERAND_INDEX_REP_MASK: u32 = 0x7;

pub const OPERAND_EXTENDED_BIT: u32 = 0x8000_0000;

// Operand types (subset of `D3D10_SB_OPERAND_TYPE`).
pub const OPERAND_TYPE_TEMP: u32 = 0;
pub const OPERAND_TYPE_INPUT: u32 = 1;
pub const OPERAND_TYPE_OUTPUT: u32 = 2;
pub const OPERAND_TYPE_IMMEDIATE32: u32 = 4;
pub const OPERAND_TYPE_SAMPLER: u32 = 6;
pub const OPERAND_TYPE_RESOURCE: u32 = 7;
pub const OPERAND_TYPE_CONSTANT_BUFFER: u32 = 8;
/// Pixel shader depth output (`oDepth`).
///
/// Upstream: `D3D10_SB_OPERAND_TYPE_OUTPUT_DEPTH`.
pub const OPERAND_TYPE_OUTPUT_DEPTH: u32 = 12;
/// Unordered access view (u#).
///
/// Upstream: `D3D11_SB_OPERAND_TYPE_UNORDERED_ACCESS_VIEW`.
pub const OPERAND_TYPE_UNORDERED_ACCESS_VIEW: u32 = 30;
/// Pixel shader depth output with a conservative depth contract (`oDepthGE`).
///
/// Upstream: `D3D11_SB_OPERAND_TYPE_OUTPUT_DEPTH_GREATER_EQUAL`.
pub const OPERAND_TYPE_OUTPUT_DEPTH_GREATER_EQUAL: u32 = 38;
/// Pixel shader depth output with a conservative depth contract (`oDepthLE`).
///
/// Upstream: `D3D11_SB_OPERAND_TYPE_OUTPUT_DEPTH_LESS_EQUAL`.
pub const OPERAND_TYPE_OUTPUT_DEPTH_LESS_EQUAL: u32 = 39;

// Index dimensions (subset of `D3D10_SB_OPERAND_INDEX_DIMENSION`).
pub const OPERAND_INDEX_DIMENSION_0D: u32 = 0;
pub const OPERAND_INDEX_DIMENSION_1D: u32 = 1;
pub const OPERAND_INDEX_DIMENSION_2D: u32 = 2;

// Index representation (subset of `D3D10_SB_OPERAND_INDEX_REPRESENTATION`).
pub const OPERAND_INDEX_REP_IMMEDIATE32: u32 = 0;

// 4-component selection modes (subset of `D3D10_SB_OPERAND_4_COMPONENT_SELECTION_MODE`).
pub const OPERAND_SEL_MASK: u32 = 0;
pub const OPERAND_SEL_SWIZZLE: u32 = 1;
pub const OPERAND_SEL_SELECT1: u32 = 2;

/// Returns the human-friendly name for an SM4/SM5 opcode.
///
/// The DXBC token stream encodes opcodes as numeric values. For bring-up work it's
/// useful to turn "unknown opcode 75" into a disassembly-style mnemonic like
/// "resinfo".
///
/// This table is intentionally small and is only used for diagnostics. It is
/// expected to grow over time as more instructions are supported.
pub fn opcode_name(opcode: u32) -> Option<&'static str> {
    match opcode {
        // ---- Opcodes implemented by the current decoder/translator ----
        OPCODE_NOP => Some("nop"),
        OPCODE_MOV => Some("mov"),
        OPCODE_MOVC => Some("movc"),
        OPCODE_ADD => Some("add"),
        OPCODE_MAD => Some("mad"),
        OPCODE_MUL => Some("mul"),
        OPCODE_RCP => Some("rcp"),
        OPCODE_RSQ => Some("rsq"),
        OPCODE_DP3 => Some("dp3"),
        OPCODE_DP4 => Some("dp4"),
        OPCODE_MIN => Some("min"),
        OPCODE_MAX => Some("max"),
        OPCODE_IABS => Some("iabs"),
        OPCODE_INEG => Some("ineg"),
        OPCODE_IMIN => Some("imin"),
        OPCODE_IMAX => Some("imax"),
        OPCODE_UMIN => Some("umin"),
        OPCODE_UMAX => Some("umax"),
        OPCODE_IEQ => Some("ieq"),
        OPCODE_IGE => Some("ige"),
        OPCODE_ILT => Some("ilt"),
        OPCODE_INE => Some("ine"),
        OPCODE_ULT => Some("ult"),
        OPCODE_UGE => Some("uge"),
        OPCODE_IADDC => Some("iaddc"),
        OPCODE_UADDC => Some("uaddc"),
        OPCODE_ISUBC => Some("isubc"),
        OPCODE_USUBB => Some("usubb"),
        OPCODE_UDIV => Some("udiv"),
        OPCODE_IDIV => Some("idiv"),
        OPCODE_BFI => Some("bfi"),
        OPCODE_UBFE => Some("ubfe"),
        OPCODE_IBFE => Some("ibfe"),
        OPCODE_CUSTOMDATA => Some("customdata"),
        OPCODE_BREAK => Some("break"),
        OPCODE_SWITCH => Some("switch"),
        OPCODE_CASE => Some("case"),
        OPCODE_DEFAULT => Some("default"),
        OPCODE_ENDSWITCH => Some("endswitch"),
        OPCODE_IF => Some("if"),
        OPCODE_ELSE => Some("else"),
        OPCODE_ENDIF => Some("endif"),
        OPCODE_RET => Some("ret"),
        OPCODE_EMIT => Some("emit"),
        OPCODE_CUT => Some("cut"),
        OPCODE_EMIT_STREAM => Some("emit_stream"),
        OPCODE_CUT_STREAM => Some("cut_stream"),
        OPCODE_SAMPLE => Some("sample"),
        OPCODE_SAMPLE_L => Some("sample_l"),
        OPCODE_LD => Some("ld"),
        OPCODE_LD_RAW => Some("ld_raw"),
        OPCODE_LD_STRUCTURED => Some("ld_structured"),
        OPCODE_STORE_RAW => Some("store_raw"),
        OPCODE_STORE_STRUCTURED => Some("store_structured"),
        OPCODE_BFREV => Some("bfrev"),
        OPCODE_COUNTBITS => Some("countbits"),
        OPCODE_FIRSTBIT_HI => Some("firstbit_hi"),
        OPCODE_FIRSTBIT_LO => Some("firstbit_lo"),
        OPCODE_FIRSTBIT_SHI => Some("firstbit_shi"),
        OPCODE_SYNC => Some("sync"),
        OPCODE_DCL_THREAD_GROUP => Some("dcl_thread_group"),
        OPCODE_DCL_GS_INPUT_PRIMITIVE => Some("dcl_gs_input_primitive"),
        OPCODE_DCL_GS_OUTPUT_TOPOLOGY => Some("dcl_gs_output_topology"),
        OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT => Some("dcl_gs_max_output_vertex_count"),
        OPCODE_DCL_GS_INSTANCE_COUNT => Some("dcl_gs_instance_count"),
        OPCODE_DCL_RESOURCE_RAW => Some("dcl_resource_raw"),
        OPCODE_DCL_RESOURCE_STRUCTURED => Some("dcl_resource_structured"),
        OPCODE_DCL_UAV_RAW => Some("dcl_uav_raw"),
        OPCODE_DCL_UAV_STRUCTURED => Some("dcl_uav_structured"),

        // ---- Common SM4/SM5 opcodes we don't translate yet (diagnostics only) ----
        // Integer/bit ops not modeled by the IR yet.
        30 => Some("iadd"),
        // Resource query.
        75 => Some("resinfo"),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_name_includes_switch_ops() {
        assert_eq!(opcode_name(OPCODE_BREAK), Some("break"));
        assert_eq!(opcode_name(OPCODE_SWITCH), Some("switch"));
        assert_eq!(opcode_name(OPCODE_CASE), Some("case"));
        assert_eq!(opcode_name(OPCODE_DEFAULT), Some("default"));
        assert_eq!(opcode_name(OPCODE_ENDSWITCH), Some("endswitch"));
    }

    #[test]
    fn opcode_name_includes_bit_utils_ops() {
        assert_eq!(opcode_name(OPCODE_BFREV), Some("bfrev"));
        assert_eq!(opcode_name(OPCODE_COUNTBITS), Some("countbits"));
        assert_eq!(opcode_name(OPCODE_FIRSTBIT_HI), Some("firstbit_hi"));
        assert_eq!(opcode_name(OPCODE_FIRSTBIT_LO), Some("firstbit_lo"));
        assert_eq!(opcode_name(OPCODE_FIRSTBIT_SHI), Some("firstbit_shi"));
    }

    #[test]
    fn opcode_name_includes_integer_ops() {
        assert_eq!(opcode_name(OPCODE_IABS), Some("iabs"));
        assert_eq!(opcode_name(OPCODE_INEG), Some("ineg"));
        assert_eq!(opcode_name(OPCODE_IMIN), Some("imin"));
        assert_eq!(opcode_name(OPCODE_IMAX), Some("imax"));
        assert_eq!(opcode_name(OPCODE_UMIN), Some("umin"));
        assert_eq!(opcode_name(OPCODE_UMAX), Some("umax"));
        assert_eq!(opcode_name(OPCODE_IEQ), Some("ieq"));
        assert_eq!(opcode_name(OPCODE_IGE), Some("ige"));
        assert_eq!(opcode_name(OPCODE_ILT), Some("ilt"));
        assert_eq!(opcode_name(OPCODE_INE), Some("ine"));
        assert_eq!(opcode_name(OPCODE_ULT), Some("ult"));
        assert_eq!(opcode_name(OPCODE_UGE), Some("uge"));
        assert_eq!(opcode_name(OPCODE_SYNC), Some("sync"));
    }
}
