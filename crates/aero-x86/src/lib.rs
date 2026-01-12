//! x86/x86-64 decoding helpers.
//!
//! The project uses `iced-x86` as the underlying decoder, but we keep a small
//! wrapper API so the rest of the emulator does not depend on `iced-x86`
//! directly.

use aero_cpu_decoder::{decode_instruction, DecodeMode};

pub use aero_cpu_decoder::{Code, Instruction, MemorySize, Mnemonic, OpKind, Register};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    InvalidInstruction,
}

#[derive(Debug, Clone)]
pub struct DecodedInst {
    pub instr: Instruction,
    pub len: u8,
}

pub fn decode(bytes: &[u8], ip: u64, bitness: u32) -> Result<DecodedInst, DecodeError> {
    let mode = match bitness {
        16 => DecodeMode::Bits16,
        32 => DecodeMode::Bits32,
        64 => DecodeMode::Bits64,
        _ => return Err(DecodeError::InvalidInstruction),
    };

    let instr = decode_instruction(mode, ip, bytes).map_err(|_| DecodeError::InvalidInstruction)?;
    Ok(DecodedInst {
        len: instr.len() as u8,
        instr,
    })
}

pub mod tier1 {
    //! Minimal Tier-1 decode / normalization layer.
    //!
    //! This module exists primarily to support the Tier-1 JIT front-end unit tests
    //! without requiring the full interpreter decode pipeline.
    //!
    //! This decoder only supports a subset of x86-64 sufficient for building and
    //! testing basic-block discovery + translation. It is **not** intended to be
    //! complete or particularly fast.

    use aero_types::{Cond, Gpr, Width};
    use core::fmt;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reg {
        pub gpr: Gpr,
        pub width: Width,
        pub high8: bool,
    }

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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Address {
        pub base: Option<Gpr>,
        pub index: Option<Gpr>,
        pub scale: u8,
        pub disp: i32,
        pub rip_relative: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Operand {
        Reg(Reg),
        Imm(u64),
        Mem(Address),
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AluOp {
        Add,
        Sub,
        And,
        Or,
        Xor,
        Shl,
        Shr,
        Sar,
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ShiftOp {
        Shl,
        Shr,
        Sar,
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum InstKind {
        Mov {
            dst: Operand,
            src: Operand,
            width: Width,
        },
        Lea {
            dst: Reg,
            addr: Address,
            width: Width,
        },
        Alu {
            op: AluOp,
            dst: Operand,
            src: Operand,
            width: Width,
        },
        Shift {
            op: ShiftOp,
            dst: Operand,
            count: u8,
            width: Width,
        },
        Cmp {
            lhs: Operand,
            rhs: Operand,
            width: Width,
        },
        Test {
            lhs: Operand,
            rhs: Operand,
            width: Width,
        },
        Inc {
            dst: Operand,
            width: Width,
        },
        Dec {
            dst: Operand,
            width: Width,
        },
        Push {
            src: Operand,
        },
        Pop {
            dst: Operand,
        },
        JmpRel {
            target: u64,
        },
        JccRel {
            cond: Cond,
            target: u64,
        },
        CallRel {
            target: u64,
        },
        Ret,
        Setcc {
            cond: Cond,
            dst: Operand,
        },
        Cmovcc {
            cond: Cond,
            dst: Reg,
            src: Operand,
            width: Width,
        },
        Invalid,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DecodedInst {
        pub rip: u64,
        pub len: u8,
        pub kind: InstKind,
    }

    impl DecodedInst {
        #[must_use]
        pub fn next_rip(&self) -> u64 {
            self.rip + self.len as u64
        }

        #[must_use]
        pub fn is_block_terminator(&self) -> bool {
            matches!(
                self.kind,
                InstKind::JmpRel { .. }
                    | InstKind::JccRel { .. }
                    | InstKind::CallRel { .. }
                    | InstKind::Ret
                    | InstKind::Invalid
            )
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DecodeError {
        pub message: &'static str,
    }

    impl fmt::Display for DecodeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for DecodeError {}

    #[derive(Debug, Clone, Copy)]
    struct Rex {
        present: bool,
        w: bool,
        r: bool,
        x: bool,
        b: bool,
    }

    impl Rex {
        fn none() -> Self {
            Self {
                present: false,
                w: false,
                r: false,
                x: false,
                b: false,
            }
        }

        fn from_byte(b: u8) -> Self {
            debug_assert!((0x40..=0x4f).contains(&b));
            Self {
                present: true,
                w: (b & 0x08) != 0,
                r: (b & 0x04) != 0,
                x: (b & 0x02) != 0,
                b: (b & 0x01) != 0,
            }
        }
    }

    fn read_u8(bytes: &[u8], offset: usize) -> Result<u8, DecodeError> {
        bytes.get(offset).copied().ok_or(DecodeError {
            message: "unexpected EOF",
        })
    }

    fn read_le(bytes: &[u8], offset: usize, len: usize) -> Result<u64, DecodeError> {
        if bytes.len() < offset + len {
            return Err(DecodeError {
                message: "unexpected EOF",
            });
        }
        let mut out = 0u64;
        for i in 0..len {
            out |= (bytes[offset + i] as u64) << (i * 8);
        }
        Ok(out)
    }

    fn decode_gpr(code: u8) -> Result<Gpr, DecodeError> {
        Gpr::from_u4(code).ok_or(DecodeError {
            message: "invalid register encoding",
        })
    }

    fn decode_reg8(code: u8, rex_present: bool) -> Result<(Gpr, bool), DecodeError> {
        let code = code & 0x0f;
        if code >= 8 {
            return Ok((decode_gpr(code)?, false));
        }
        match (code, rex_present) {
            (0..=3, _) => Ok((decode_gpr(code)?, false)),
            (4..=7, true) => Ok((decode_gpr(code)?, false)),
            (4..=7, false) => Ok((decode_gpr(code - 4)?, true)),
            _ => Err(DecodeError {
                message: "invalid 8-bit register encoding",
            }),
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct ModRm {
        mod_bits: u8,
        reg: u8,
        rm: u8,
    }

    fn parse_modrm(byte: u8, rex: Rex) -> ModRm {
        let mod_bits = (byte >> 6) & 0x3;
        let reg = ((byte >> 3) & 0x7) | if rex.r { 8 } else { 0 };
        let rm = (byte & 0x7) | if rex.b { 8 } else { 0 };
        ModRm { mod_bits, reg, rm }
    }

    fn parse_sib(byte: u8, rex: Rex) -> (u8, u8, u8) {
        let scale_bits = (byte >> 6) & 0x3;
        let index = ((byte >> 3) & 0x7) | if rex.x { 8 } else { 0 };
        let base = (byte & 0x7) | if rex.b { 8 } else { 0 };
        (scale_bits, index, base)
    }

    fn op_width(bitness: u32, rex: Rex, operand_override: bool) -> Width {
        match bitness {
            64 => {
                if rex.w {
                    Width::W64
                } else if operand_override {
                    Width::W16
                } else {
                    Width::W32
                }
            }
            32 => {
                if operand_override {
                    Width::W16
                } else {
                    Width::W32
                }
            }
            16 => {
                if operand_override {
                    Width::W32
                } else {
                    Width::W16
                }
            }
            _ => Width::W32,
        }
    }

    fn sign_extend_imm(width: Width, imm: u64) -> u64 {
        width.sign_extend(width.truncate(imm))
    }

    fn decode_modrm_operand(
        bytes: &[u8],
        offset: &mut usize,
        bitness: u32,
        rex: Rex,
        rex_present: bool,
        width: Width,
    ) -> Result<(Operand, ModRm), DecodeError> {
        let modrm_byte = read_u8(bytes, *offset)?;
        *offset += 1;
        let modrm = parse_modrm(modrm_byte, rex);
        if modrm.mod_bits == 3 {
            let reg = if width == Width::W8 {
                let (gpr, high8) = decode_reg8(modrm.rm, rex_present)?;
                Reg { gpr, width, high8 }
            } else {
                Reg {
                    gpr: decode_gpr(modrm.rm)?,
                    width,
                    high8: false,
                }
            };
            return Ok((Operand::Reg(reg), modrm));
        }

        // Memory operand.
        let mut base: Option<Gpr> = None;
        let mut index: Option<Gpr> = None;
        let mut scale: u8 = 1;
        let mut disp: i32 = 0;
        let mut rip_relative = false;

        let rm_low3 = modrm.rm & 0x7;
        if rm_low3 == 4 {
            let sib_byte = read_u8(bytes, *offset)?;
            *offset += 1;
            let (scale_bits, index_code, base_code) = parse_sib(sib_byte, rex);
            scale = 1u8 << scale_bits;
            if (index_code & 0x7) != 4 {
                index = Some(decode_gpr(index_code)?);
            }
            if (base_code & 0x7) == 5 && modrm.mod_bits == 0 {
                base = None;
            } else {
                base = Some(decode_gpr(base_code)?);
            }
            if (base_code & 0x7) == 5 && modrm.mod_bits == 0 && base.is_none() {
                // No base, disp32 follows.
                let disp32 = read_le(bytes, *offset, 4)? as u32;
                *offset += 4;
                disp = disp32 as i32;
            }
        } else if rm_low3 == 5 && modrm.mod_bits == 0 {
            if bitness == 64 {
                // RIP-relative (64-bit mode).
                rip_relative = true;
                let disp32 = read_le(bytes, *offset, 4)? as u32;
                *offset += 4;
                disp = disp32 as i32;
            } else {
                // Absolute disp32 addressing (32-bit mode). 16-bit mode uses a different
                // addressing scheme which is not supported by this Tier1 decoder.
                base = None;
                let disp32 = read_le(bytes, *offset, 4)? as u32;
                *offset += 4;
                disp = disp32 as i32;
            }
        } else {
            base = Some(decode_gpr(modrm.rm)?);
        }

        match modrm.mod_bits {
            0 => {}
            1 => {
                let d8 = read_u8(bytes, *offset)? as i8;
                *offset += 1;
                disp = disp.wrapping_add(d8 as i32);
            }
            2 => {
                let d32 = read_le(bytes, *offset, 4)? as u32;
                *offset += 4;
                disp = disp.wrapping_add(d32 as i32);
            }
            _ => unreachable!(),
        }

        Ok((
            Operand::Mem(Address {
                base,
                index,
                scale,
                disp,
                rip_relative,
            }),
            modrm,
        ))
    }

    fn decode_reg_from_modrm(
        modrm: ModRm,
        rex_present: bool,
        width: Width,
    ) -> Result<Reg, DecodeError> {
        if width == Width::W8 {
            let (gpr, high8) = decode_reg8(modrm.reg, rex_present)?;
            Ok(Reg { gpr, width, high8 })
        } else {
            Ok(Reg {
                gpr: decode_gpr(modrm.reg)?,
                width,
                high8: false,
            })
        }
    }

    /// Decode a single instruction at `rip` from `bytes`.
    ///
    /// The caller is expected to provide up to 15 bytes (the architectural maximum
    /// length). If decoding fails, an [`InstKind::Invalid`] instruction is returned
    /// with a conservative 1-byte length so front-ends can always make progress.
    #[must_use]
    pub fn decode_one(rip: u64, bytes: &[u8]) -> DecodedInst {
        decode_one_mode(rip, bytes, 64)
    }

    /// Decode a single instruction at `rip` from `bytes`, using the requested x86 bitness.
    ///
    /// `bitness` must be one of 16, 32, or 64.
    #[must_use]
    pub fn decode_one_mode(rip: u64, bytes: &[u8], bitness: u32) -> DecodedInst {
        match decode_one_inner(rip, bytes, bitness) {
            Ok(inst) => inst,
            Err(_) => DecodedInst {
                rip,
                len: 1,
                kind: InstKind::Invalid,
            },
        }
    }

    fn decode_one_inner(rip: u64, bytes: &[u8], bitness: u32) -> Result<DecodedInst, DecodeError> {
        if !matches!(bitness, 16 | 32 | 64) {
            return Err(DecodeError {
                message: "unsupported bitness",
            });
        }

        let mut offset = 0usize;
        let mut rex = Rex::none();
        let mut operand_override = false;

        loop {
            let b = read_u8(bytes, offset)?;
            match b {
                0x66 => {
                    operand_override = true;
                    offset += 1;
                }
                0xf2 | 0xf3 | 0x67 => {
                    // Ignored for this subset.
                    offset += 1;
                }
                0x40..=0x4f if bitness == 64 => {
                    rex = Rex::from_byte(b);
                    offset += 1;
                }
                _ => break,
            }
        }

        let opcode1 = read_u8(bytes, offset)?;
        offset += 1;

        let width = op_width(bitness, rex, operand_override);

        let kind = match opcode1 {
            0x40..=0x47 if bitness != 64 => {
                let reg_code = opcode1 - 0x40;
                let w = op_width(bitness, Rex::none(), operand_override);
                InstKind::Inc {
                    dst: Operand::Reg(Reg {
                        gpr: decode_gpr(reg_code)?,
                        width: w,
                        high8: false,
                    }),
                    width: w,
                }
            }
            0x48..=0x4f if bitness != 64 => {
                let reg_code = opcode1 - 0x48;
                let w = op_width(bitness, Rex::none(), operand_override);
                InstKind::Dec {
                    dst: Operand::Reg(Reg {
                        gpr: decode_gpr(reg_code)?,
                        width: w,
                        high8: false,
                    }),
                    width: w,
                }
            }
            0xb8..=0xbf => {
                let reg_code = (opcode1 - 0xb8) | if rex.b { 8 } else { 0 };
                let dst = Reg {
                    gpr: decode_gpr(reg_code)?,
                    width,
                    high8: false,
                };
                let imm_len = width.bytes();
                let imm = read_le(bytes, offset, imm_len)?;
                offset += imm_len;
                InstKind::Mov {
                    dst: Operand::Reg(dst),
                    src: Operand::Imm(imm),
                    width,
                }
            }
            0xb0..=0xb7 => {
                let reg_code = (opcode1 - 0xb0) | if rex.b { 8 } else { 0 };
                let (gpr, high8) = decode_reg8(reg_code, rex.present)?;
                let dst = Reg {
                    gpr,
                    width: Width::W8,
                    high8,
                };
                let imm = read_u8(bytes, offset)? as u64;
                offset += 1;
                InstKind::Mov {
                    dst: Operand::Reg(dst),
                    src: Operand::Imm(imm),
                    width: Width::W8,
                }
            }
            0x89 | 0x88 => {
                let w = if opcode1 == 0x88 { Width::W8 } else { width };
                let (dst, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, w)?;
                let src_reg = decode_reg_from_modrm(modrm, rex.present, w)?;
                InstKind::Mov {
                    dst,
                    src: Operand::Reg(src_reg),
                    width: w,
                }
            }
            0x8b | 0x8a => {
                let w = if opcode1 == 0x8a { Width::W8 } else { width };
                let (src, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, w)?;
                let dst_reg = decode_reg_from_modrm(modrm, rex.present, w)?;
                InstKind::Mov {
                    dst: Operand::Reg(dst_reg),
                    src,
                    width: w,
                }
            }
            0xc7 | 0xc6 => {
                let w = if opcode1 == 0xc6 { Width::W8 } else { width };
                let (dst, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, w)?;
                let group = modrm.reg & 0x7;
                if group != 0 {
                    return Err(DecodeError {
                        message: "unsupported group for C6/C7",
                    });
                }
                let imm_len = if w == Width::W16 {
                    2
                } else if w == Width::W8 {
                    1
                } else {
                    4
                };
                let imm_raw = read_le(bytes, offset, imm_len)?;
                offset += imm_len;
                let imm = if w == Width::W64 && imm_len == 4 {
                    sign_extend_imm(Width::W32, imm_raw)
                } else if imm_len == 1 && w != Width::W8 {
                    sign_extend_imm(Width::W8, imm_raw)
                } else {
                    w.truncate(imm_raw)
                };
                InstKind::Mov {
                    dst,
                    src: Operand::Imm(imm),
                    width: w,
                }
            }
            0x8d => {
                let (src, modrm) = decode_modrm_operand(
                    bytes,
                    &mut offset,
                    bitness,
                    rex,
                    rex.present,
                    Width::W64,
                )?;
                let Operand::Mem(addr) = src else {
                    return Err(DecodeError {
                        message: "LEA requires memory operand",
                    });
                };
                let dst = decode_reg_from_modrm(modrm, rex.present, width)?;
                InstKind::Lea { dst, addr, width }
            }
            0x01 | 0x03 | 0x21 | 0x23 | 0x09 | 0x0b | 0x31 | 0x33 | 0x29 | 0x2b | 0x39 | 0x3b
            | 0x85 => {
                let (rm_op, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, width)?;
                let reg_op = Operand::Reg(decode_reg_from_modrm(modrm, rex.present, width)?);

                let (op, is_cmp, is_test, dst, src) = match opcode1 {
                    0x01 => (AluOp::Add, false, false, rm_op, reg_op),
                    0x03 => (AluOp::Add, false, false, reg_op, rm_op),
                    0x21 => (AluOp::And, false, false, rm_op, reg_op),
                    0x23 => (AluOp::And, false, false, reg_op, rm_op),
                    0x09 => (AluOp::Or, false, false, rm_op, reg_op),
                    0x0b => (AluOp::Or, false, false, reg_op, rm_op),
                    0x31 => (AluOp::Xor, false, false, rm_op, reg_op),
                    0x33 => (AluOp::Xor, false, false, reg_op, rm_op),
                    0x29 => (AluOp::Sub, false, false, rm_op, reg_op),
                    0x2b => (AluOp::Sub, false, false, reg_op, rm_op),
                    0x39 => (AluOp::Sub, true, false, rm_op, reg_op),
                    0x3b => (AluOp::Sub, true, false, reg_op, rm_op),
                    0x85 => (AluOp::And, false, true, rm_op, reg_op),
                    _ => unreachable!(),
                };

                if is_cmp {
                    InstKind::Cmp {
                        lhs: dst,
                        rhs: src,
                        width,
                    }
                } else if is_test {
                    InstKind::Test {
                        lhs: dst,
                        rhs: src,
                        width,
                    }
                } else {
                    InstKind::Alu {
                        op,
                        dst,
                        src,
                        width,
                    }
                }
            }
            // Group2 shifts (0xC0/0xC1/0xD0/0xD1) are decoded below as `InstKind::Shift` so Tier1
            // can keep the shift count as a `u8` instead of embedding it into an `Operand::Imm`.
            0x05 | 0x25 | 0x0d | 0x35 | 0x2d | 0x3d | 0xa9 => {
                let (op, is_cmp, is_test, acc) = match opcode1 {
                    0x05 => (AluOp::Add, false, false, Gpr::Rax),
                    0x25 => (AluOp::And, false, false, Gpr::Rax),
                    0x0d => (AluOp::Or, false, false, Gpr::Rax),
                    0x35 => (AluOp::Xor, false, false, Gpr::Rax),
                    0x2d => (AluOp::Sub, false, false, Gpr::Rax),
                    0x3d => (AluOp::Sub, true, false, Gpr::Rax),
                    0xa9 => (AluOp::And, false, true, Gpr::Rax),
                    _ => unreachable!(),
                };
                let imm32 = read_le(bytes, offset, 4)? as u32;
                offset += 4;
                let imm = if width == Width::W64 {
                    sign_extend_imm(Width::W32, imm32 as u64)
                } else {
                    imm32 as u64
                };
                let acc_reg = Operand::Reg(Reg {
                    gpr: acc,
                    width,
                    high8: false,
                });
                if is_cmp {
                    InstKind::Cmp {
                        lhs: acc_reg,
                        rhs: Operand::Imm(imm),
                        width,
                    }
                } else if is_test {
                    InstKind::Test {
                        lhs: acc_reg,
                        rhs: Operand::Imm(imm),
                        width,
                    }
                } else {
                    InstKind::Alu {
                        op,
                        dst: acc_reg,
                        src: Operand::Imm(imm),
                        width,
                    }
                }
            }
            0x81 | 0x83 => {
                let (dst, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, width)?;
                let group = modrm.reg & 0x7;
                let imm = if opcode1 == 0x83 {
                    let imm8 = read_u8(bytes, offset)? as i8 as i64 as u64;
                    offset += 1;
                    width.truncate(imm8)
                } else {
                    let imm32 = read_le(bytes, offset, 4)? as u32;
                    offset += 4;
                    if width == Width::W64 {
                        sign_extend_imm(Width::W32, imm32 as u64)
                    } else {
                        imm32 as u64
                    }
                };

                match group {
                    0 => InstKind::Alu {
                        op: AluOp::Add,
                        dst,
                        src: Operand::Imm(imm),
                        width,
                    },
                    1 => InstKind::Alu {
                        op: AluOp::Or,
                        dst,
                        src: Operand::Imm(imm),
                        width,
                    },
                    4 => InstKind::Alu {
                        op: AluOp::And,
                        dst,
                        src: Operand::Imm(imm),
                        width,
                    },
                    5 => InstKind::Alu {
                        op: AluOp::Sub,
                        dst,
                        src: Operand::Imm(imm),
                        width,
                    },
                    6 => InstKind::Alu {
                        op: AluOp::Xor,
                        dst,
                        src: Operand::Imm(imm),
                        width,
                    },
                    7 => InstKind::Cmp {
                        lhs: dst,
                        rhs: Operand::Imm(imm),
                        width,
                    },
                    _ => {
                        return Err(DecodeError {
                            message: "unsupported 0x81/0x83 group",
                        })
                    }
                }
            }
            0xd1 => {
                let (dst, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, width)?;
                let group = modrm.reg & 0x7;
                let op = match group {
                    4 => ShiftOp::Shl,
                    5 => ShiftOp::Shr,
                    7 => ShiftOp::Sar,
                    _ => {
                        return Err(DecodeError {
                            message: "unsupported 0xD1 group",
                        })
                    }
                };
                InstKind::Shift {
                    op,
                    dst,
                    count: 1,
                    width,
                }
            }
            0xd0 => {
                let (dst, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, Width::W8)?;
                let group = modrm.reg & 0x7;
                let op = match group {
                    4 => ShiftOp::Shl,
                    5 => ShiftOp::Shr,
                    7 => ShiftOp::Sar,
                    _ => {
                        return Err(DecodeError {
                            message: "unsupported 0xD0 group",
                        })
                    }
                };
                InstKind::Shift {
                    op,
                    dst,
                    count: 1,
                    width: Width::W8,
                }
            }
            0xc1 => {
                let (dst, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, width)?;
                let group = modrm.reg & 0x7;
                let op = match group {
                    4 => ShiftOp::Shl,
                    5 => ShiftOp::Shr,
                    7 => ShiftOp::Sar,
                    _ => {
                        return Err(DecodeError {
                            message: "unsupported 0xC1 group",
                        })
                    }
                };
                let imm8 = read_u8(bytes, offset)?;
                offset += 1;
                InstKind::Shift {
                    op,
                    dst,
                    count: imm8,
                    width,
                }
            }
            0xc0 => {
                let (dst, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, Width::W8)?;
                let group = modrm.reg & 0x7;
                let op = match group {
                    4 => ShiftOp::Shl,
                    5 => ShiftOp::Shr,
                    7 => ShiftOp::Sar,
                    _ => {
                        return Err(DecodeError {
                            message: "unsupported 0xC0 group",
                        })
                    }
                };
                let imm8 = read_u8(bytes, offset)?;
                offset += 1;
                InstKind::Shift {
                    op,
                    dst,
                    count: imm8,
                    width: Width::W8,
                }
            }
            0xff => {
                let (opnd, modrm) =
                    decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, Width::W64)?;
                let group = modrm.reg & 0x7;
                match group {
                    0 => InstKind::Inc {
                        dst: opnd,
                        width: Width::W64,
                    },
                    1 => InstKind::Dec {
                        dst: opnd,
                        width: Width::W64,
                    },
                    6 => InstKind::Push { src: opnd },
                    _ => {
                        return Err(DecodeError {
                            message: "unsupported 0xFF group",
                        })
                    }
                }
            }
            0x50..=0x57 => {
                let reg_code = (opcode1 - 0x50) | if rex.b { 8 } else { 0 };
                let reg = Operand::Reg(Reg {
                    gpr: decode_gpr(reg_code)?,
                    width: Width::W64,
                    high8: false,
                });
                InstKind::Push { src: reg }
            }
            0x58..=0x5f => {
                let reg_code = (opcode1 - 0x58) | if rex.b { 8 } else { 0 };
                let reg = Operand::Reg(Reg {
                    gpr: decode_gpr(reg_code)?,
                    width: Width::W64,
                    high8: false,
                });
                InstKind::Pop { dst: reg }
            }
            0x6a => {
                let imm8 = read_u8(bytes, offset)? as i8 as i64 as u64;
                offset += 1;
                InstKind::Push {
                    src: Operand::Imm(imm8),
                }
            }
            0x68 => {
                let imm32 = read_le(bytes, offset, 4)? as u32;
                offset += 4;
                let imm64 = sign_extend_imm(Width::W32, imm32 as u64);
                InstKind::Push {
                    src: Operand::Imm(imm64),
                }
            }
            0xe9 => {
                let rel32 = read_le(bytes, offset, 4)? as u32;
                offset += 4;
                let target = (rip + offset as u64).wrapping_add(rel32 as i32 as i64 as u64);
                InstKind::JmpRel { target }
            }
            0xeb => {
                let rel8 = read_u8(bytes, offset)? as i8;
                offset += 1;
                let target = (rip + offset as u64).wrapping_add(rel8 as i64 as u64);
                InstKind::JmpRel { target }
            }
            0xe8 => {
                let rel32 = read_le(bytes, offset, 4)? as u32;
                offset += 4;
                let target = (rip + offset as u64).wrapping_add(rel32 as i32 as i64 as u64);
                InstKind::CallRel { target }
            }
            0xc3 => InstKind::Ret,
            0x70..=0x7f => {
                let cc = opcode1 - 0x70;
                let cond = Cond::from_cc(cc).ok_or(DecodeError {
                    message: "invalid condition code",
                })?;
                let rel8 = read_u8(bytes, offset)? as i8;
                offset += 1;
                let target = (rip + offset as u64).wrapping_add(rel8 as i64 as u64);
                InstKind::JccRel { cond, target }
            }
            0x0f => {
                let opcode2 = read_u8(bytes, offset)?;
                offset += 1;
                match opcode2 {
                    0x80..=0x8f => {
                        let cc = opcode2 - 0x80;
                        let cond = Cond::from_cc(cc).ok_or(DecodeError {
                            message: "invalid condition code",
                        })?;
                        let rel32 = read_le(bytes, offset, 4)? as u32;
                        offset += 4;
                        let target = (rip + offset as u64).wrapping_add(rel32 as i32 as i64 as u64);
                        InstKind::JccRel { cond, target }
                    }
                    0x90..=0x9f => {
                        let cc = opcode2 - 0x90;
                        let cond = Cond::from_cc(cc).ok_or(DecodeError {
                            message: "invalid condition code",
                        })?;
                        let (dst, _modrm) = decode_modrm_operand(
                            bytes,
                            &mut offset,
                            bitness,
                            rex,
                            rex.present,
                            Width::W8,
                        )?;
                        InstKind::Setcc { cond, dst }
                    }
                    0x40..=0x4f => {
                        let cc = opcode2 - 0x40;
                        let cond = Cond::from_cc(cc).ok_or(DecodeError {
                            message: "invalid condition code",
                        })?;
                        let (src, modrm) =
                            decode_modrm_operand(bytes, &mut offset, bitness, rex, rex.present, width)?;
                        let dst = decode_reg_from_modrm(modrm, rex.present, width)?;
                        InstKind::Cmovcc {
                            cond,
                            dst,
                            src,
                            width,
                        }
                    }
                    _ => {
                        return Err(DecodeError {
                            message: "unsupported 0F xx opcode",
                        })
                    }
                }
            }
            _ => {
                return Err(DecodeError {
                    message: "unsupported opcode",
                })
            }
        };

        Ok(DecodedInst {
            rip,
            len: offset as u8,
            kind,
        })
    }
}

/// Production-oriented instruction decoder and operand model.
pub mod decoder;
pub mod inst;
pub mod opcode_tables;
