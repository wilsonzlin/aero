use crate::cpu::Reg;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rex {
    w: bool,
    r: bool,
    x: bool,
    b: bool,
}

impl Rex {
    #[inline]
    fn parse(byte: u8) -> Option<Self> {
        if !(0x40..=0x4F).contains(&byte) {
            return None;
        }
        Some(Self {
            w: (byte & 0x08) != 0,
            r: (byte & 0x04) != 0,
            x: (byte & 0x02) != 0,
            b: (byte & 0x01) != 0,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemOperand {
    pub base: Option<Reg>,
    pub index: Option<Reg>,
    pub scale: u8,
    pub disp: i32,
    pub rip_relative: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand64 {
    Reg(Reg),
    Imm(i64),
    Mem(MemOperand),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstKind {
    Mov64 {
        dst: Operand64,
        src: Operand64,
    },
    Add64 {
        dst: Operand64,
        src: Operand64,
    },
    Sub64 {
        dst: Operand64,
        src: Operand64,
    },
    Cmp64 {
        lhs: Operand64,
        rhs: Operand64,
    },
    Jmp {
        target: u64,
    },
    Jcc {
        cond: Cond,
        target: u64,
        fallthrough: u64,
    },
    Ret,
    Hlt,
    Nop,
}

impl InstKind {
    pub fn is_control_flow(&self) -> bool {
        matches!(
            self,
            InstKind::Jmp { .. } | InstKind::Jcc { .. } | InstKind::Ret | InstKind::Hlt
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedInst {
    pub rip: u64,
    pub len: u8,
    pub kind: InstKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    UnsupportedOpcode(u8),
    UnsupportedEncoding(&'static str),
}

#[derive(Clone, Debug, Default)]
pub struct Decoder;

impl Decoder {
    pub fn decode(&self, bytes: &[u8], rip: u64) -> Result<DecodedInst, DecodeError> {
        let mut idx = 0usize;

        let rex = if bytes.get(idx).copied().and_then(Rex::parse).is_some() {
            let rex = Rex::parse(bytes[idx]).unwrap();
            idx += 1;
            rex
        } else {
            Rex::default()
        };

        let opcode = *bytes.get(idx).ok_or(DecodeError::Truncated)?;
        idx += 1;

        let kind = match opcode {
            0x90 => InstKind::Nop,
            0xF4 => InstKind::Hlt,
            0xC3 => InstKind::Ret,
            0xEB => {
                let rel = *bytes.get(idx).ok_or(DecodeError::Truncated)? as i8;
                idx += 1;
                let fallthrough = rip.wrapping_add(idx as u64);
                let target = (fallthrough as i64).wrapping_add(rel as i64) as u64;
                InstKind::Jmp { target }
            }
            0xE9 => {
                let rel = read_i32(bytes, &mut idx)?;
                let fallthrough = rip.wrapping_add(idx as u64);
                let target = (fallthrough as i64).wrapping_add(rel as i64) as u64;
                InstKind::Jmp { target }
            }
            0x74 | 0x75 => {
                let rel = *bytes.get(idx).ok_or(DecodeError::Truncated)? as i8;
                idx += 1;
                let fallthrough = rip.wrapping_add(idx as u64);
                let target = (fallthrough as i64).wrapping_add(rel as i64) as u64;
                let cond = if opcode == 0x74 { Cond::Eq } else { Cond::Ne };
                InstKind::Jcc {
                    cond,
                    target,
                    fallthrough,
                }
            }
            0x0F => {
                let op2 = *bytes.get(idx).ok_or(DecodeError::Truncated)?;
                idx += 1;
                match op2 {
                    0x84 | 0x85 => {
                        let rel = read_i32(bytes, &mut idx)?;
                        let fallthrough = rip.wrapping_add(idx as u64);
                        let target = (fallthrough as i64).wrapping_add(rel as i64) as u64;
                        let cond = if op2 == 0x84 { Cond::Eq } else { Cond::Ne };
                        InstKind::Jcc {
                            cond,
                            target,
                            fallthrough,
                        }
                    }
                    _ => return Err(DecodeError::UnsupportedOpcode(op2)),
                }
            }
            0xB8..=0xBF => {
                if !rex.w {
                    return Err(DecodeError::UnsupportedEncoding(
                        "mov r64, imm requires REX.W=1",
                    ));
                }
                let reg_id = (opcode - 0xB8) | if rex.b { 8 } else { 0 };
                let reg =
                    Reg::from_u4(reg_id).ok_or(DecodeError::UnsupportedEncoding("bad reg"))?;

                let imm = read_u64(bytes, &mut idx)? as i64;

                InstKind::Mov64 {
                    dst: Operand64::Reg(reg),
                    src: Operand64::Imm(imm),
                }
            }
            0x89 | 0x8B | 0x01 | 0x03 | 0x29 | 0x2B | 0x39 | 0x3B | 0x83 => {
                if !rex.w {
                    return Err(DecodeError::UnsupportedEncoding(
                        "only 64-bit ops supported (REX.W=1)",
                    ));
                }
                let modrm = *bytes.get(idx).ok_or(DecodeError::Truncated)?;
                idx += 1;

                let mod_bits = (modrm >> 6) & 0b11;
                let reg_bits = (modrm >> 3) & 0b111;
                let rm_bits = modrm & 0b111;

                let reg_id = reg_bits | if rex.r { 8 } else { 0 };
                let rm_id = rm_bits | if rex.b { 8 } else { 0 };

                let reg =
                    Reg::from_u4(reg_id).ok_or(DecodeError::UnsupportedEncoding("bad reg"))?;

                let rm_operand = if mod_bits == 0b11 {
                    let rm =
                        Reg::from_u4(rm_id).ok_or(DecodeError::UnsupportedEncoding("bad rm"))?;
                    Operand64::Reg(rm)
                } else {
                    let mem = parse_mem(bytes, &mut idx, rex, mod_bits, rm_bits, rm_id)?;
                    Operand64::Mem(mem)
                };

                match opcode {
                    0x89 => InstKind::Mov64 {
                        dst: rm_operand,
                        src: Operand64::Reg(reg),
                    },
                    0x8B => InstKind::Mov64 {
                        dst: Operand64::Reg(reg),
                        src: rm_operand,
                    },
                    0x01 => InstKind::Add64 {
                        dst: rm_operand,
                        src: Operand64::Reg(reg),
                    },
                    0x03 => InstKind::Add64 {
                        dst: Operand64::Reg(reg),
                        src: rm_operand,
                    },
                    0x29 => InstKind::Sub64 {
                        dst: rm_operand,
                        src: Operand64::Reg(reg),
                    },
                    0x2B => InstKind::Sub64 {
                        dst: Operand64::Reg(reg),
                        src: rm_operand,
                    },
                    0x39 => InstKind::Cmp64 {
                        lhs: rm_operand,
                        rhs: Operand64::Reg(reg),
                    },
                    0x3B => InstKind::Cmp64 {
                        lhs: Operand64::Reg(reg),
                        rhs: rm_operand,
                    },
                    0x83 => {
                        let imm = *bytes.get(idx).ok_or(DecodeError::Truncated)? as i8 as i64;
                        idx += 1;
                        match reg_bits {
                            0 => InstKind::Add64 {
                                dst: rm_operand,
                                src: Operand64::Imm(imm),
                            },
                            5 => InstKind::Sub64 {
                                dst: rm_operand,
                                src: Operand64::Imm(imm),
                            },
                            7 => InstKind::Cmp64 {
                                lhs: rm_operand,
                                rhs: Operand64::Imm(imm),
                            },
                            _ => {
                                return Err(DecodeError::UnsupportedEncoding(
                                    "unsupported group1 /reg in opcode 0x83",
                                ))
                            }
                        }
                    }
                    _ => return Err(DecodeError::UnsupportedOpcode(opcode)),
                }
            }
            _ => return Err(DecodeError::UnsupportedOpcode(opcode)),
        };

        Ok(DecodedInst {
            rip,
            len: idx as u8,
            kind,
        })
    }
}

fn read_u64(bytes: &[u8], idx: &mut usize) -> Result<u64, DecodeError> {
    let tail = bytes.get(*idx..*idx + 8).ok_or(DecodeError::Truncated)?;
    *idx += 8;
    Ok(u64::from_le_bytes(tail.try_into().unwrap()))
}

fn read_i32(bytes: &[u8], idx: &mut usize) -> Result<i32, DecodeError> {
    let tail = bytes.get(*idx..*idx + 4).ok_or(DecodeError::Truncated)?;
    *idx += 4;
    Ok(i32::from_le_bytes(tail.try_into().unwrap()))
}

fn parse_mem(
    bytes: &[u8],
    idx: &mut usize,
    rex: Rex,
    mod_bits: u8,
    rm_bits: u8,
    rm_id: u8,
) -> Result<MemOperand, DecodeError> {
    debug_assert!(mod_bits != 0b11);

    // SIB byte.
    let mut base: Option<Reg> = None;
    let mut index: Option<Reg> = None;
    let mut scale = 1u8;
    let disp: i32;
    let mut rip_relative = false;

    if rm_bits == 0b100 {
        let sib = *bytes.get(*idx).ok_or(DecodeError::Truncated)?;
        *idx += 1;

        let scale_bits = (sib >> 6) & 0b11;
        let index_bits = (sib >> 3) & 0b111;
        let base_bits = sib & 0b111;

        scale = 1u8 << scale_bits;

        if index_bits != 0b100 {
            let index_id = index_bits | if rex.x { 8 } else { 0 };
            index = Some(
                Reg::from_u4(index_id).ok_or(DecodeError::UnsupportedEncoding("bad sib index"))?,
            );
        }

        if mod_bits == 0 && base_bits == 0b101 {
            // disp32 with no base (absolute address).
            base = None;
            disp = read_i32(bytes, idx)?;
        } else {
            let base_id = base_bits | if rex.b { 8 } else { 0 };
            base = Some(
                Reg::from_u4(base_id).ok_or(DecodeError::UnsupportedEncoding("bad sib base"))?,
            );
            disp = read_disp(bytes, idx, mod_bits)?;
        }
    } else if mod_bits == 0 && rm_bits == 0b101 {
        // RIP-relative disp32.
        rip_relative = true;
        disp = read_i32(bytes, idx)?;
    } else {
        base = Some(Reg::from_u4(rm_id).ok_or(DecodeError::UnsupportedEncoding("bad base"))?);
        disp = read_disp(bytes, idx, mod_bits)?;
    }

    Ok(MemOperand {
        base,
        index,
        scale,
        disp,
        rip_relative,
    })
}

fn read_disp(bytes: &[u8], idx: &mut usize, mod_bits: u8) -> Result<i32, DecodeError> {
    match mod_bits {
        0b00 => Ok(0),
        0b01 => {
            let d = *bytes.get(*idx).ok_or(DecodeError::Truncated)? as i8 as i32;
            *idx += 1;
            Ok(d)
        }
        0b10 => read_i32(bytes, idx),
        _ => Ok(0),
    }
}
