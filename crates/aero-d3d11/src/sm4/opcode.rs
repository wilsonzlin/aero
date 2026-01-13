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

/// `bfi` (bitfield insert).
pub const OPCODE_BFI: u32 = 0x67;
/// `ubfe` (unsigned bitfield extract).
pub const OPCODE_UBFE: u32 = 0x68;
/// `ibfe` (signed bitfield extract).
pub const OPCODE_IBFE: u32 = 0x69;

/// Non-executable custom data / comment block.
///
/// Used for embedded comments, debug data, and immediate constant buffers.
pub const OPCODE_CUSTOMDATA: u32 = 0x1f;

pub const OPCODE_RET: u32 = 0x3e;

// Geometry shader stream emission / cutting.
//
// Values from the D3D10+ tokenized shader format opcode table:
// `D3D10_SB_OPCODE_TYPE_EMIT`, `D3D10_SB_OPCODE_TYPE_CUT`,
// `D3D10_SB_OPCODE_TYPE_EMIT_STREAM`, `D3D10_SB_OPCODE_TYPE_CUT_STREAM`
// in the Windows SDK header `d3d10tokenizedprogramformat.h`.
pub const OPCODE_EMIT: u32 = 0x3f;
pub const OPCODE_CUT: u32 = 0x40;
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
/// Unordered access view (u#).
///
/// Upstream: `D3D11_SB_OPERAND_TYPE_UNORDERED_ACCESS_VIEW`.
pub const OPERAND_TYPE_UNORDERED_ACCESS_VIEW: u32 = 30;

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
