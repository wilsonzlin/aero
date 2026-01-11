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

pub const OPCODE_MOV: u32 = 0x01;
pub const OPCODE_ADD: u32 = 0x02;
pub const OPCODE_MAD: u32 = 0x04;
pub const OPCODE_MUL: u32 = 0x05;
pub const OPCODE_RCP: u32 = 0x06;
pub const OPCODE_RSQ: u32 = 0x07;
pub const OPCODE_DP3: u32 = 0x08;
pub const OPCODE_DP4: u32 = 0x09;
pub const OPCODE_MIN: u32 = 0x0a;
pub const OPCODE_MAX: u32 = 0x0b;

pub const OPCODE_RET: u32 = 0x3e;

// Texture ops are not used by the bootstrap translator yet; include likely
// numeric IDs but also support structural detection in the decoder.
pub const OPCODE_SAMPLE: u32 = 0x45;
pub const OPCODE_SAMPLE_L: u32 = 0x46;

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
