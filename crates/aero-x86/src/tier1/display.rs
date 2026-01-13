use super::{AluOp, DecodeError, Operand, Reg, ShiftOp};
use aero_types::{Gpr, Width};
use core::fmt;

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.width == Width::W8 {
            if self.high8 {
                let s = match self.gpr {
                    Gpr::Rax => "ah",
                    Gpr::Rcx => "ch",
                    Gpr::Rdx => "dh",
                    Gpr::Rbx => "bh",
                    _ => "??",
                };
                return f.write_str(s);
            }
            let s = match self.gpr {
                Gpr::Rax => "al",
                Gpr::Rcx => "cl",
                Gpr::Rdx => "dl",
                Gpr::Rbx => "bl",
                Gpr::Rsp => "spl",
                Gpr::Rbp => "bpl",
                Gpr::Rsi => "sil",
                Gpr::Rdi => "dil",
                Gpr::R8 => "r8b",
                Gpr::R9 => "r9b",
                Gpr::R10 => "r10b",
                Gpr::R11 => "r11b",
                Gpr::R12 => "r12b",
                Gpr::R13 => "r13b",
                Gpr::R14 => "r14b",
                Gpr::R15 => "r15b",
            };
            return f.write_str(s);
        }
        if self.width == Width::W16 {
            let s = match self.gpr {
                Gpr::Rax => "ax",
                Gpr::Rcx => "cx",
                Gpr::Rdx => "dx",
                Gpr::Rbx => "bx",
                Gpr::Rsp => "sp",
                Gpr::Rbp => "bp",
                Gpr::Rsi => "si",
                Gpr::Rdi => "di",
                Gpr::R8 => "r8w",
                Gpr::R9 => "r9w",
                Gpr::R10 => "r10w",
                Gpr::R11 => "r11w",
                Gpr::R12 => "r12w",
                Gpr::R13 => "r13w",
                Gpr::R14 => "r14w",
                Gpr::R15 => "r15w",
            };
            return f.write_str(s);
        }
        if self.width == Width::W32 {
            let s = match self.gpr {
                Gpr::Rax => "eax",
                Gpr::Rcx => "ecx",
                Gpr::Rdx => "edx",
                Gpr::Rbx => "ebx",
                Gpr::Rsp => "esp",
                Gpr::Rbp => "ebp",
                Gpr::Rsi => "esi",
                Gpr::Rdi => "edi",
                Gpr::R8 => "r8d",
                Gpr::R9 => "r9d",
                Gpr::R10 => "r10d",
                Gpr::R11 => "r11d",
                Gpr::R12 => "r12d",
                Gpr::R13 => "r13d",
                Gpr::R14 => "r14d",
                Gpr::R15 => "r15d",
            };
            return f.write_str(s);
        }
        write!(f, "{}", self.gpr)
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Reg(r) => write!(f, "{r}"),
            Operand::Imm(v) => write!(f, "0x{v:x}"),
            Operand::Mem(addr) => {
                f.write_str("[")?;
                let mut first = true;
                if addr.rip_relative {
                    f.write_str("rip")?;
                    first = false;
                }
                if let Some(base) = addr.base {
                    if !first {
                        f.write_str("+")?;
                    }
                    write!(f, "{base}")?;
                    first = false;
                }
                if let Some(index) = addr.index {
                    if !first {
                        f.write_str("+")?;
                    }
                    write!(f, "{index}")?;
                    if addr.scale != 1 {
                        write!(f, "*{}", addr.scale)?;
                    }
                    first = false;
                }
                if addr.disp != 0 || first {
                    if !first && addr.disp >= 0 {
                        f.write_str("+")?;
                    }
                    write!(f, "{}", addr.disp)?;
                }
                f.write_str("]")?;
                Ok(())
            }
        }
    }
}

impl fmt::Display for ShiftOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ShiftOp::Shl => "shl",
            ShiftOp::Shr => "shr",
            ShiftOp::Sar => "sar",
        };
        f.write_str(s)
    }
}

impl fmt::Display for AluOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AluOp::Add => "add",
            AluOp::Sub => "sub",
            AluOp::And => "and",
            AluOp::Or => "or",
            AluOp::Xor => "xor",
            AluOp::Shl => "shl",
            AluOp::Shr => "shr",
            AluOp::Sar => "sar",
        };
        f.write_str(s)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
