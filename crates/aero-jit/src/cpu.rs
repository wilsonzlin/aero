use core::fmt;

/// Architectural general-purpose registers (GPRs).
///
/// The register numbering matches the x86 ModRM encoding (lower 3 bits), with
/// the high bit coming from REX.{B,R,X} where applicable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Reg {
    #[inline]
    pub const fn from_u4(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Rax),
            1 => Some(Self::Rcx),
            2 => Some(Self::Rdx),
            3 => Some(Self::Rbx),
            4 => Some(Self::Rsp),
            5 => Some(Self::Rbp),
            6 => Some(Self::Rsi),
            7 => Some(Self::Rdi),
            8 => Some(Self::R8),
            9 => Some(Self::R9),
            10 => Some(Self::R10),
            11 => Some(Self::R11),
            12 => Some(Self::R12),
            13 => Some(Self::R13),
            14 => Some(Self::R14),
            15 => Some(Self::R15),
            _ => None,
        }
    }

    #[inline]
    pub const fn as_usize(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Flag {
    Cf,
    Zf,
    Sf,
    Of,
}

pub const FLAGS_CF: u64 = 1 << 0;
pub const FLAGS_ZF: u64 = 1 << 6;
pub const FLAGS_SF: u64 = 1 << 7;
pub const FLAGS_OF: u64 = 1 << 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FlagOp {
    Add = 1,
    Sub = 2,
    Logic = 3,
}

impl fmt::Display for FlagOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlagOp::Add => f.write_str("add"),
            FlagOp::Sub => f.write_str("sub"),
            FlagOp::Logic => f.write_str("logic"),
        }
    }
}

/// A lazily-evaluated flags record.
///
/// Instead of eagerly computing the full x86 flags word after every ALU
/// operation, we store the operands and operation type, and compute the flags
/// on demand when a flag is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct PendingFlags {
    /// 0 = invalid, 1 = valid.
    pub valid: u8,
    pub op: u8,
    /// Operand width in bits: 8/16/32/64.
    pub width_bits: u8,
    pub _pad: u8,
    pub lhs: u64,
    pub rhs: u64,
    pub result: u64,
}

impl PendingFlags {
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.valid != 0
    }

    #[inline]
    pub fn set(&mut self, op: FlagOp, width_bits: u8, lhs: u64, rhs: u64, result: u64) {
        debug_assert!(matches!(width_bits, 8 | 16 | 32 | 64));
        self.valid = 1;
        self.op = op as u8;
        self.width_bits = width_bits;
        self.lhs = lhs;
        self.rhs = rhs;
        self.result = result;
    }

    #[inline]
    pub fn invalidate(&mut self) {
        self.valid = 0;
        self.op = 0;
        self.width_bits = 0;
        self.lhs = 0;
        self.rhs = 0;
        self.result = 0;
    }
}

/// Minimal architectural CPU state shared between the interpreter and the JIT.
///
/// This is intentionally "flat": the baseline JIT codegen treats this struct
/// as a blob in linear memory and loads/stores fields by fixed offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct CpuState {
    pub regs: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub pending_flags: PendingFlags,
    pub halted: u8,
    pub _pad: [u8; 7],
}

impl CpuState {
    #[inline]
    pub fn reg(&self, reg: Reg) -> u64 {
        self.regs[reg.as_usize()]
    }

    #[inline]
    pub fn set_reg(&mut self, reg: Reg, value: u64) {
        self.regs[reg.as_usize()] = value;
    }

    #[inline]
    pub fn set_halted(&mut self) {
        self.halted = 1;
    }

    #[inline]
    pub fn is_halted(&self) -> bool {
        self.halted != 0
    }

    #[inline]
    pub fn set_pending_flags(
        &mut self,
        op: FlagOp,
        width_bits: u8,
        lhs: u64,
        rhs: u64,
        result: u64,
    ) {
        self.pending_flags.set(op, width_bits, lhs, rhs, result);
    }

    #[inline]
    pub fn clear_pending_flags(&mut self) {
        self.pending_flags.invalidate();
    }

    /// Read a flag, materializing pending flags into `rflags` if needed.
    ///
    /// This matches the "lazy flags" mechanism expected by both interpreter and
    /// baseline JIT: ALU ops set `pending_flags`, and conditional branches read
    /// flags through this method.
    #[inline]
    pub fn read_flag(&mut self, flag: Flag) -> bool {
        if self.pending_flags.is_valid() {
            self.materialize_pending_flags();
        }

        let mask = match flag {
            Flag::Cf => FLAGS_CF,
            Flag::Zf => FLAGS_ZF,
            Flag::Sf => FLAGS_SF,
            Flag::Of => FLAGS_OF,
        };
        (self.rflags & mask) != 0
    }

    fn materialize_pending_flags(&mut self) {
        let pf = self.pending_flags;
        let width_bits = pf.width_bits;
        let mask = if width_bits == 64 {
            u64::MAX
        } else {
            (1u64 << width_bits) - 1
        };

        let lhs = pf.lhs & mask;
        let rhs = pf.rhs & mask;
        let result = pf.result & mask;

        let sign_bit = 1u64 << (width_bits - 1);

        let mut rflags = self.rflags;
        rflags &= !(FLAGS_CF | FLAGS_ZF | FLAGS_SF | FLAGS_OF);

        // ZF, SF are common across most ops we care about.
        if result == 0 {
            rflags |= FLAGS_ZF;
        }
        if (result & sign_bit) != 0 {
            rflags |= FLAGS_SF;
        }

        let op = match pf.op {
            x if x == FlagOp::Add as u8 => FlagOp::Add,
            x if x == FlagOp::Sub as u8 => FlagOp::Sub,
            x if x == FlagOp::Logic as u8 => FlagOp::Logic,
            _ => FlagOp::Logic,
        };

        match op {
            FlagOp::Add => {
                if result < lhs {
                    rflags |= FLAGS_CF;
                }
                // Signed overflow: if inputs have same sign and result differs.
                let overflow = ((lhs ^ result) & (rhs ^ result) & sign_bit) != 0;
                if overflow {
                    rflags |= FLAGS_OF;
                }
            }
            FlagOp::Sub => {
                if lhs < rhs {
                    rflags |= FLAGS_CF;
                }
                // Signed overflow on subtraction: if lhs and rhs signs differ and result differs from lhs.
                let overflow = ((lhs ^ rhs) & (lhs ^ result) & sign_bit) != 0;
                if overflow {
                    rflags |= FLAGS_OF;
                }
            }
            FlagOp::Logic => {
                // CF/OF cleared.
            }
        }

        self.rflags = rflags;
        self.pending_flags.invalidate();
    }
}
