//! Shared, dependency-light types used across Aero crates.
//!
//! The real Aero project will eventually need far more CPU state and ISA
//! surface area. For now we keep these definitions intentionally small so
//! the Tier-1 JIT front-end can be implemented and tested in isolation.

use core::fmt;

/// The bit-width of an integer value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Width {
    W8,
    W16,
    W32,
    W64,
}

impl Width {
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Width::W8 => 8,
            Width::W16 => 16,
            Width::W32 => 32,
            Width::W64 => 64,
        }
    }

    #[must_use]
    pub const fn bytes(self) -> usize {
        (self.bits() / 8) as usize
    }

    #[must_use]
    pub const fn mask(self) -> u64 {
        match self {
            Width::W8 => 0xff,
            Width::W16 => 0xffff,
            Width::W32 => 0xffff_ffff,
            Width::W64 => u64::MAX,
        }
    }

    #[must_use]
    pub const fn truncate(self, value: u64) -> u64 {
        value & self.mask()
    }

    /// Sign-extend `value` (which is assumed to already be truncated to `self`)
    /// to 64 bits.
    #[must_use]
    pub const fn sign_extend(self, value: u64) -> u64 {
        match self {
            Width::W8 => (value as i8 as i64) as u64,
            Width::W16 => (value as i16 as i64) as u64,
            Width::W32 => (value as i32 as i64) as u64,
            Width::W64 => value,
        }
    }
}

impl fmt::Display for Width {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Width::W8 => write!(f, "i8"),
            Width::W16 => write!(f, "i16"),
            Width::W32 => write!(f, "i32"),
            Width::W64 => write!(f, "i64"),
        }
    }
}

/// x86-64 general-purpose registers in architectural order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Gpr {
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

impl Gpr {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub const fn from_u4(v: u8) -> Option<Self> {
        match v {
            0 => Some(Gpr::Rax),
            1 => Some(Gpr::Rcx),
            2 => Some(Gpr::Rdx),
            3 => Some(Gpr::Rbx),
            4 => Some(Gpr::Rsp),
            5 => Some(Gpr::Rbp),
            6 => Some(Gpr::Rsi),
            7 => Some(Gpr::Rdi),
            8 => Some(Gpr::R8),
            9 => Some(Gpr::R9),
            10 => Some(Gpr::R10),
            11 => Some(Gpr::R11),
            12 => Some(Gpr::R12),
            13 => Some(Gpr::R13),
            14 => Some(Gpr::R14),
            15 => Some(Gpr::R15),
            _ => None,
        }
    }
}

impl fmt::Display for Gpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Gpr::Rax => "rax",
            Gpr::Rcx => "rcx",
            Gpr::Rdx => "rdx",
            Gpr::Rbx => "rbx",
            Gpr::Rsp => "rsp",
            Gpr::Rbp => "rbp",
            Gpr::Rsi => "rsi",
            Gpr::Rdi => "rdi",
            Gpr::R8 => "r8",
            Gpr::R9 => "r9",
            Gpr::R10 => "r10",
            Gpr::R11 => "r11",
            Gpr::R12 => "r12",
            Gpr::R13 => "r13",
            Gpr::R14 => "r14",
            Gpr::R15 => "r15",
        };
        f.write_str(s)
    }
}

/// Subset of EFLAGS/RFLAGS bits used by Tier-1 translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    Cf,
    Pf,
    Af,
    Zf,
    Sf,
    Of,
}

impl Flag {
    #[must_use]
    pub const fn rflags_bit(self) -> u8 {
        match self {
            Flag::Cf => 0,
            Flag::Pf => 2,
            Flag::Af => 4,
            Flag::Zf => 6,
            Flag::Sf => 7,
            Flag::Of => 11,
        }
    }
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Flag::Cf => "CF",
            Flag::Pf => "PF",
            Flag::Af => "AF",
            Flag::Zf => "ZF",
            Flag::Sf => "SF",
            Flag::Of => "OF",
        };
        f.write_str(s)
    }
}

/// A compact set of flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FlagSet(u8);

impl FlagSet {
    pub const EMPTY: FlagSet = FlagSet(0);
    pub const CF: FlagSet = FlagSet(1 << 0);
    pub const PF: FlagSet = FlagSet(1 << 1);
    pub const AF: FlagSet = FlagSet(1 << 2);
    pub const ZF: FlagSet = FlagSet(1 << 3);
    pub const SF: FlagSet = FlagSet(1 << 4);
    pub const OF: FlagSet = FlagSet(1 << 5);

    pub const ALU: FlagSet = FlagSet(
        FlagSet::CF.0 | FlagSet::PF.0 | FlagSet::AF.0 | FlagSet::ZF.0 | FlagSet::SF.0 | FlagSet::OF.0,
    );

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn contains(self, other: FlagSet) -> bool {
        (self.0 & other.0) == other.0
    }

    #[must_use]
    pub const fn union(self, other: FlagSet) -> FlagSet {
        FlagSet(self.0 | other.0)
    }

    #[must_use]
    pub const fn without(self, other: FlagSet) -> FlagSet {
        FlagSet(self.0 & !other.0)
    }

    #[must_use]
    pub const fn iter(self) -> FlagSetIter {
        FlagSetIter { set: self, idx: 0 }
    }
}

impl fmt::Display for FlagSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("∅");
        }
        let mut first = true;
        for flag in self.iter() {
            if !first {
                f.write_str("|")?;
            }
            first = false;
            write!(f, "{flag}")?;
        }
        Ok(())
    }
}

pub struct FlagSetIter {
    set: FlagSet,
    idx: u8,
}

impl Iterator for FlagSetIter {
    type Item = Flag;

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < 6 {
            let bit = 1u8 << self.idx;
            let idx = self.idx;
            self.idx += 1;
            if (self.set.0 & bit) == 0 {
                continue;
            }
            return Some(match idx {
                0 => Flag::Cf,
                1 => Flag::Pf,
                2 => Flag::Af,
                3 => Flag::Zf,
                4 => Flag::Sf,
                5 => Flag::Of,
                _ => unreachable!(),
            });
        }
        None
    }
}

/// x86 condition codes as used by Jcc/SETcc/CMOVcc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cond {
    O,
    No,
    B,
    Ae,
    E,
    Ne,
    Be,
    A,
    S,
    Ns,
    P,
    Np,
    L,
    Ge,
    Le,
    G,
}

impl Cond {
    /// Decode a condition code from the low 4 bits of an x86 opcode.
    #[must_use]
    pub const fn from_cc(cc: u8) -> Option<Self> {
        match cc & 0x0f {
            0x0 => Some(Cond::O),
            0x1 => Some(Cond::No),
            0x2 => Some(Cond::B),
            0x3 => Some(Cond::Ae),
            0x4 => Some(Cond::E),
            0x5 => Some(Cond::Ne),
            0x6 => Some(Cond::Be),
            0x7 => Some(Cond::A),
            0x8 => Some(Cond::S),
            0x9 => Some(Cond::Ns),
            0xa => Some(Cond::P),
            0xb => Some(Cond::Np),
            0xc => Some(Cond::L),
            0xd => Some(Cond::Ge),
            0xe => Some(Cond::Le),
            0xf => Some(Cond::G),
            _ => None,
        }
    }

    #[must_use]
    pub const fn uses_flags(self) -> FlagSet {
        match self {
            Cond::O | Cond::No => FlagSet::OF,
            Cond::B | Cond::Ae => FlagSet::CF,
            Cond::E | Cond::Ne => FlagSet::ZF,
            Cond::Be | Cond::A => FlagSet::CF.union(FlagSet::ZF),
            Cond::S | Cond::Ns => FlagSet::SF,
            Cond::P | Cond::Np => FlagSet::PF,
            Cond::L | Cond::Ge => FlagSet::SF.union(FlagSet::OF),
            Cond::Le | Cond::G => FlagSet::ZF.union(FlagSet::SF.union(FlagSet::OF)),
        }
    }

    #[must_use]
    pub const fn eval(self, cf: bool, pf: bool, zf: bool, sf: bool, of: bool) -> bool {
        match self {
            Cond::O => of,
            Cond::No => !of,
            Cond::B => cf,
            Cond::Ae => !cf,
            Cond::E => zf,
            Cond::Ne => !zf,
            Cond::Be => cf || zf,
            Cond::A => !cf && !zf,
            Cond::S => sf,
            Cond::Ns => !sf,
            Cond::P => pf,
            Cond::Np => !pf,
            Cond::L => sf != of,
            Cond::Ge => sf == of,
            Cond::Le => zf || (sf != of),
            Cond::G => !zf && (sf == of),
        }
    }
}

impl fmt::Display for Cond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Cond::O => "o",
            Cond::No => "no",
            Cond::B => "b",
            Cond::Ae => "ae",
            Cond::E => "e",
            Cond::Ne => "ne",
            Cond::Be => "be",
            Cond::A => "a",
            Cond::S => "s",
            Cond::Ns => "ns",
            Cond::P => "p",
            Cond::Np => "np",
            Cond::L => "l",
            Cond::Ge => "ge",
            Cond::Le => "le",
            Cond::G => "g",
        };
        f.write_str(s)
    }
}

