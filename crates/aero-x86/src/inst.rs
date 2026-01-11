//! Instruction and operand model produced by the decoder.
//!
//! This is intentionally a *lossy* representation compared to the full x86 ISA: it focuses on
//! information needed for correct instruction fetching, effective address computation, control-flow
//! analysis, and JIT block formation.

use core::fmt;

/// Effective decoding mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeMode {
    /// 16-bit real mode / 16-bit code segment defaults.
    Bits16,
    /// 32-bit protected mode / 32-bit code segment defaults.
    Bits32,
    /// 64-bit long mode.
    Bits64,
}

/// Operand size in bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandSize {
    Bits8,
    Bits16,
    Bits32,
    Bits64,
    Bits80,
    Bits128,
    Bits256,
}

impl OperandSize {
    #[must_use]
    pub const fn bits(self) -> u16 {
        match self {
            Self::Bits8 => 8,
            Self::Bits16 => 16,
            Self::Bits32 => 32,
            Self::Bits64 => 64,
            Self::Bits80 => 80,
            Self::Bits128 => 128,
            Self::Bits256 => 256,
        }
    }
}

/// Address size in bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSize {
    Bits16,
    Bits32,
    Bits64,
}

impl AddressSize {
    #[must_use]
    pub const fn bits(self) -> u16 {
        match self {
            Self::Bits16 => 16,
            Self::Bits32 => 32,
            Self::Bits64 => 64,
        }
    }
}

/// Segment register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentReg {
    ES,
    CS,
    SS,
    DS,
    FS,
    GS,
}

/// REP/REPNZ prefix kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepPrefix {
    Rep,
    Repne,
}

/// A parsed REX prefix (64-bit mode only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RexPrefix {
    pub w: bool,
    pub r: bool,
    pub x: bool,
    pub b: bool,
}

/// Legacy prefixes (and REX in long mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Prefixes {
    pub lock: bool,
    pub rep: Option<RepPrefix>,
    pub segment: Option<SegmentReg>,
    pub operand_size_override: bool,
    pub address_size_override: bool,
    /// Present iff any REX prefix was seen.
    pub rex: Option<RexPrefix>,
}

/// Opcode map used by legacy encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpcodeMap {
    /// One-byte opcode map.
    Primary,
    /// Two-byte opcode map (`0F xx`).
    Map0F,
    /// Three-byte opcode map (`0F 38 xx`).
    Map0F38,
    /// Three-byte opcode map (`0F 3A xx`).
    Map0F3A,
    /// VEX/EVEX/XOP (not fully decoded yet).
    Extended,
}

/// Raw opcode bytes after prefix scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpcodeBytes {
    pub map: OpcodeMap,
    /// The final opcode byte (e.g. for `0F 84`, `opcode=0x84`).
    pub opcode: u8,
    /// `Some(reg)` for group opcodes where ModRM.reg selects a sub-op.
    pub opcode_ext: Option<u8>,
}

/// A decoded instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedInst {
    /// Instruction length in bytes.
    pub length: u8,
    pub opcode: OpcodeBytes,
    pub prefixes: Prefixes,
    pub operand_size: OperandSize,
    pub address_size: AddressSize,
    pub operands: Vec<Operand>,
    pub flags: InstFlags,
}

/// High-level properties of an instruction, used for block formation and privilege checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InstFlags {
    pub is_branch: bool,
    pub is_call: bool,
    pub is_ret: bool,
    pub is_privileged: bool,
    pub reads_flags: bool,
    pub writes_flags: bool,
}

/// A general-purpose register reference.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gpr {
    /// Register number: `0..=15` for `A..R15`.
    pub index: u8,
}

impl fmt::Debug for Gpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Gpr({})", self.index)
    }
}

/// An XMM register reference.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Xmm {
    pub index: u8,
}

impl fmt::Debug for Xmm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Xmm({})", self.index)
    }
}

/// An instruction operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Gpr {
        reg: Gpr,
        size: OperandSize,
        /// True for AH/CH/DH/BH (only possible for 8-bit operands without REX).
        high8: bool,
    },
    Xmm {
        reg: Xmm,
    },
    /// A register operand whose class isn't modelled yet (e.g. x87 `ST0`, MMX `MM0`, YMM, etc).
    OtherReg {
        class: OtherRegClass,
        index: u8,
    },
    Segment {
        reg: SegmentReg,
    },
    Control {
        index: u8,
    },
    Debug {
        index: u8,
    },
    Memory(MemoryOperand),
    Immediate(Immediate),
    /// A relative branch target, computed as `next_ip + rel`.
    Relative {
        target: u64,
        size: OperandSize,
    },
}

/// Register classes that are not yet explicitly represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherRegClass {
    /// x87 floating-point stack regs (ST0..ST7).
    Fpu,
    /// MMX regs (MM0..MM7).
    Mmx,
    /// YMM regs (AVX).
    Ymm,
    /// ZMM regs (AVX-512).
    Zmm,
    /// Opmask regs (k0..k7).
    Mask,
    /// Anything else.
    Unknown,
}

/// Memory addressing metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryOperand {
    pub segment: Option<SegmentReg>,
    pub addr_size: AddressSize,
    pub base: Option<Gpr>,
    pub index: Option<Gpr>,
    pub scale: u8,
    pub disp: i64,
    pub rip_relative: bool,
}

/// An immediate operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Immediate {
    pub value: u64,
    pub size: OperandSize,
    pub is_signed: bool,
}
