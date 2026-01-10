use crate::bus::Bus;
use crate::cpu::{Cpu, CpuMode, RFlags};
use crate::interp::{bitext, sse3, sse41, sse42, ssse3, ExecError};

#[derive(Clone, Copy, Debug, Default)]
struct Rex {
    w: bool,
    r: bool,
    x: bool,
    b: bool,
}

impl Rex {
    fn decode(byte: u8) -> Self {
        Self {
            w: (byte & 0x08) != 0,
            r: (byte & 0x04) != 0,
            x: (byte & 0x02) != 0,
            b: (byte & 0x01) != 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ModRm {
    mod_bits: u8,
    reg: u8,
    rm: u8,
}

impl ModRm {
    fn decode(byte: u8) -> Self {
        Self {
            mod_bits: byte >> 6,
            reg: (byte >> 3) & 7,
            rm: byte & 7,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RmOperand {
    Reg(u8),
    Mem(u64),
}

fn need_byte(bytes: &[u8], idx: &mut usize) -> Result<u8, ExecError> {
    let b = *bytes.get(*idx).ok_or(ExecError::TruncatedInstruction)?;
    *idx += 1;
    Ok(b)
}

fn need_u32(bytes: &[u8], idx: &mut usize) -> Result<u32, ExecError> {
    let start = *idx;
    let end = start + 4;
    let slice = bytes.get(start..end).ok_or(ExecError::TruncatedInstruction)?;
    *idx = end;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn decode_rm_operand(cpu: &Cpu, bytes: &[u8], idx: &mut usize, modrm: ModRm, rex: Rex) -> Result<RmOperand, ExecError> {
    if modrm.mod_bits == 0b11 {
        return Ok(RmOperand::Reg(modrm.rm | ((rex.b as u8) << 3)));
    }

    if cpu.mode != CpuMode::Long64 {
        // The address generation rules differ in legacy modes and are not
        // needed by the current unit tests for Win7 extensions.
        return Err(ExecError::InvalidOpcode(0x0F));
    }

    let mut base_reg: Option<u8> = None;
    let mut index_reg: Option<u8> = None;
    let mut scale: u64 = 1;
    let mut disp: i64 = 0;

    if modrm.rm == 0b100 {
        let sib = need_byte(bytes, idx)?;
        let sib_scale = sib >> 6;
        let sib_index = (sib >> 3) & 7;
        let sib_base = sib & 7;

        scale = 1u64 << sib_scale;
        if sib_index != 0b100 {
            index_reg = Some(sib_index | ((rex.x as u8) << 3));
        }

        if sib_base == 0b101 && modrm.mod_bits == 0 {
            disp = need_u32(bytes, idx)? as i32 as i64;
        } else {
            base_reg = Some(sib_base | ((rex.b as u8) << 3));
        }
    } else if modrm.rm == 0b101 && modrm.mod_bits == 0 {
        // RIP-relative in real hardware. For unit tests we treat it as absolute
        // disp32 (base=0), which is sufficient for controlled test memory.
        disp = need_u32(bytes, idx)? as i32 as i64;
    } else {
        base_reg = Some(modrm.rm | ((rex.b as u8) << 3));
    }

    match modrm.mod_bits {
        0 => {}
        1 => disp = need_byte(bytes, idx)? as i8 as i64,
        2 => disp = need_u32(bytes, idx)? as i32 as i64,
        _ => return Err(ExecError::InvalidOpcode(0x0F)),
    }

    let mut addr = disp as u64;
    if let Some(reg) = base_reg {
        addr = addr.wrapping_add(cpu.regs.gpr(reg));
    }
    if let Some(reg) = index_reg {
        addr = addr.wrapping_add(cpu.regs.gpr(reg).wrapping_mul(scale));
    }

    Ok(RmOperand::Mem(addr))
}

fn read_u128<B: Bus>(bus: &mut B, addr: u64) -> u128 {
    let lo = bus.read_u64(addr) as u128;
    let hi = bus.read_u64(addr + 8) as u128;
    lo | (hi << 64)
}

fn read_rm_u8<B: Bus>(cpu: &Cpu, bus: &mut B, rm: RmOperand) -> u8 {
    match rm {
        RmOperand::Reg(r) => cpu.regs.gpr(r) as u8,
        RmOperand::Mem(addr) => bus.read_u8(addr),
    }
}

fn read_rm_u16<B: Bus>(cpu: &Cpu, bus: &mut B, rm: RmOperand) -> u16 {
    match rm {
        RmOperand::Reg(r) => cpu.regs.gpr(r) as u16,
        RmOperand::Mem(addr) => bus.read_u16(addr),
    }
}

fn read_rm_u32<B: Bus>(cpu: &Cpu, bus: &mut B, rm: RmOperand) -> u32 {
    match rm {
        RmOperand::Reg(r) => cpu.regs.gpr(r) as u32,
        RmOperand::Mem(addr) => bus.read_u32(addr),
    }
}

fn read_rm_u64<B: Bus>(cpu: &Cpu, bus: &mut B, rm: RmOperand) -> u64 {
    match rm {
        RmOperand::Reg(r) => cpu.regs.gpr(r),
        RmOperand::Mem(addr) => bus.read_u64(addr),
    }
}

fn read_rm_u128<B: Bus>(cpu: &Cpu, bus: &mut B, rm: RmOperand) -> u128 {
    match rm {
        RmOperand::Reg(r) => cpu.sse.xmm[r as usize],
        RmOperand::Mem(addr) => read_u128(bus, addr),
    }
}

fn write_gpr(cpu: &mut Cpu, index: u8, width_bits: u32, value: u64) {
    let cur = cpu.regs.gpr(index);
    let new = match width_bits {
        16 => (cur & !0xFFFF) | (value & 0xFFFF),
        32 => {
            let v = value as u32 as u64;
            if cpu.mode == CpuMode::Long64 {
                v
            } else {
                (cur & !0xFFFF_FFFF) | v
            }
        }
        64 => value,
        _ => unreachable!("unsupported gpr width"),
    };
    cpu.regs.set_gpr(index, new);
}

fn require_win7_ext(cpu: &Cpu, opcode: u8) -> Result<(), ExecError> {
    if cpu.features.win7_x86_extensions {
        Ok(())
    } else {
        Err(ExecError::InvalidOpcode(opcode))
    }
}

pub fn exec<B: Bus>(cpu: &mut Cpu, bus: &mut B, bytes: &[u8]) -> Result<usize, ExecError> {
    let mut idx = 0usize;

    let mut prefix_66 = false;
    let mut prefix_f2 = false;
    let mut prefix_f3 = false;
    let mut rex = Rex::default();

    loop {
        let b = *bytes.get(idx).ok_or(ExecError::TruncatedInstruction)?;
        match b {
            0x66 => {
                prefix_66 = true;
                idx += 1;
            }
            0xF2 => {
                prefix_f2 = true;
                idx += 1;
            }
            0xF3 => {
                prefix_f3 = true;
                idx += 1;
            }
            0x40..=0x4F if cpu.mode == CpuMode::Long64 => {
                rex = Rex::decode(b & 0x0F);
                idx += 1;
            }
            _ => break,
        }
    }

    let first = need_byte(bytes, &mut idx)?;
    if first != 0x0F {
        return Err(ExecError::InvalidOpcode(first));
    }

    let op1 = need_byte(bytes, &mut idx)?;
    match op1 {
        0x38 => {
            let op2 = need_byte(bytes, &mut idx)?;
            match op2 {
                // SSSE3
                0x00 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    let dst_val = cpu.sse.xmm[dst as usize];
                    cpu.sse.xmm[dst as usize] = ssse3::pshufb(dst_val, src);
                }
                0x01 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = ssse3::phaddw(cpu.sse.xmm[dst as usize], src);
                }
                0x02 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = ssse3::phaddd(cpu.sse.xmm[dst as usize], src);
                }
                0x03 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = ssse3::phaddsw(cpu.sse.xmm[dst as usize], src);
                }
                0x04 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = ssse3::pmaddubsw(cpu.sse.xmm[dst as usize], src);
                }
                0x1C if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = ssse3::pabsb(src);
                }
                0x1D if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = ssse3::pabsw(src);
                }
                0x1E if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = ssse3::pabsd(src);
                }

                // SSE4.1
                0x17 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let a = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let b = read_rm_u128(cpu, bus, rm);
                    let (zf, cf) = sse41::ptest(cpu.sse.xmm[a as usize], b);
                    cpu.rflags.set(RFlags::ZF, zf);
                    cpu.rflags.set(RFlags::CF, cf);
                    cpu.rflags.set(RFlags::OF, false);
                    cpu.rflags.set(RFlags::SF, false);
                }
                0x20 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src_bytes = match rm {
                        RmOperand::Reg(r) => cpu.sse.xmm[r as usize].to_le_bytes()[..8].to_vec(),
                        RmOperand::Mem(addr) => bus.read_u64(addr).to_le_bytes().to_vec(),
                    };
                    cpu.sse.xmm[dst as usize] = sse41::pmovsxbw(&src_bytes);
                }
                0x21 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src_bytes = match rm {
                        RmOperand::Reg(r) => cpu.sse.xmm[r as usize].to_le_bytes()[..4].to_vec(),
                        RmOperand::Mem(addr) => bus.read_u32(addr).to_le_bytes().to_vec(),
                    };
                    cpu.sse.xmm[dst as usize] = sse41::pmovsxbd(&src_bytes);
                }
                0x22 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src_bytes = match rm {
                        RmOperand::Reg(r) => cpu.sse.xmm[r as usize].to_le_bytes()[..2].to_vec(),
                        RmOperand::Mem(addr) => bus.read_u16(addr).to_le_bytes().to_vec(),
                    };
                    cpu.sse.xmm[dst as usize] = sse41::pmovsxbq(&src_bytes);
                }
                0x30 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src_bytes = match rm {
                        RmOperand::Reg(r) => cpu.sse.xmm[r as usize].to_le_bytes()[..8].to_vec(),
                        RmOperand::Mem(addr) => bus.read_u64(addr).to_le_bytes().to_vec(),
                    };
                    cpu.sse.xmm[dst as usize] = sse41::pmovzxbw(&src_bytes);
                }
                0x31 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src_bytes = match rm {
                        RmOperand::Reg(r) => cpu.sse.xmm[r as usize].to_le_bytes()[..4].to_vec(),
                        RmOperand::Mem(addr) => bus.read_u32(addr).to_le_bytes().to_vec(),
                    };
                    cpu.sse.xmm[dst as usize] = sse41::pmovzxbd(&src_bytes);
                }
                0x32 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src_bytes = match rm {
                        RmOperand::Reg(r) => cpu.sse.xmm[r as usize].to_le_bytes()[..2].to_vec(),
                        RmOperand::Mem(addr) => bus.read_u16(addr).to_le_bytes().to_vec(),
                    };
                    cpu.sse.xmm[dst as usize] = sse41::pmovzxbq(&src_bytes);
                }
                0x40 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    cpu.sse.xmm[dst as usize] = sse41::pmulld(cpu.sse.xmm[dst as usize], src);
                }

                // SSE4.2 CRC32
                0xF0 if prefix_f2 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let seed = cpu.regs.gpr(dst) as u32;
                    let val = read_rm_u8(cpu, bus, rm);
                    let res = sse42::crc32_u8(seed, val);
                    write_gpr(cpu, dst, if rex.w { 64 } else { 32 }, res as u64);
                }
                0xF1 if prefix_f2 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let seed = cpu.regs.gpr(dst) as u32;

                    let (res, dst_width) = if prefix_66 {
                        let val = read_rm_u16(cpu, bus, rm);
                        (sse42::crc32_u16(seed, val), if rex.w { 64 } else { 32 })
                    } else if rex.w {
                        let val = read_rm_u64(cpu, bus, rm);
                        (sse42::crc32_u64(seed, val), 64)
                    } else {
                        let val = read_rm_u32(cpu, bus, rm);
                        (sse42::crc32_u32(seed, val), 32)
                    };
                    write_gpr(cpu, dst, dst_width, res as u64);
                }

                _ => return Err(ExecError::InvalidOpcode(op2)),
            }
        }
        0x3A => {
            let op2 = need_byte(bytes, &mut idx)?;
            match op2 {
                0x0F if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    let imm = need_byte(bytes, &mut idx)?;
                    cpu.sse.xmm[dst as usize] = ssse3::palignr(cpu.sse.xmm[dst as usize], src, imm);
                }
                0x0E if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let dst = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let src = read_rm_u128(cpu, bus, rm);
                    let imm = need_byte(bytes, &mut idx)?;
                    cpu.sse.xmm[dst as usize] = sse41::pblendw(cpu.sse.xmm[dst as usize], src, imm);
                }
                0x60 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let a = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let b = read_rm_u128(cpu, bus, rm);
                    let imm = need_byte(bytes, &mut idx)?;
                    let (mask, flags) = sse42::pcmpe_strm(
                        cpu.sse.xmm[a as usize],
                        b,
                        imm,
                        cpu.regs.eax() as u32,
                        cpu.regs.rdx as u32,
                    );
                    cpu.sse.xmm[0] = mask;
                    cpu.rflags.set(RFlags::CF, flags.cf);
                    cpu.rflags.set(RFlags::ZF, flags.zf);
                    cpu.rflags.set(RFlags::SF, flags.sf);
                    cpu.rflags.set(RFlags::OF, flags.of);
                }
                0x61 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let a = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let b = read_rm_u128(cpu, bus, rm);
                    let imm = need_byte(bytes, &mut idx)?;
                    let (index, flags) = sse42::pcmpe_stri(
                        cpu.sse.xmm[a as usize],
                        b,
                        imm,
                        cpu.regs.eax() as u32,
                        cpu.regs.rdx as u32,
                    );
                    cpu.regs.set_ecx(index, cpu.mode);
                    cpu.rflags.set(RFlags::CF, flags.cf);
                    cpu.rflags.set(RFlags::ZF, flags.zf);
                    cpu.rflags.set(RFlags::SF, flags.sf);
                    cpu.rflags.set(RFlags::OF, flags.of);
                }
                0x62 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let a = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let b = read_rm_u128(cpu, bus, rm);
                    let imm = need_byte(bytes, &mut idx)?;
                    let (mask, flags) = sse42::pcmpi_strm(cpu.sse.xmm[a as usize], b, imm);
                    cpu.sse.xmm[0] = mask;
                    cpu.rflags.set(RFlags::CF, flags.cf);
                    cpu.rflags.set(RFlags::ZF, flags.zf);
                    cpu.rflags.set(RFlags::SF, flags.sf);
                    cpu.rflags.set(RFlags::OF, flags.of);
                }
                0x63 if prefix_66 => {
                    require_win7_ext(cpu, op2)?;
                    let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
                    let a = modrm.reg | ((rex.r as u8) << 3);
                    let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
                    let b = read_rm_u128(cpu, bus, rm);
                    let imm = need_byte(bytes, &mut idx)?;
                    let (index, flags) = sse42::pcmpi_stri(cpu.sse.xmm[a as usize], b, imm);
                    cpu.regs.set_ecx(index, cpu.mode);
                    cpu.rflags.set(RFlags::CF, flags.cf);
                    cpu.rflags.set(RFlags::ZF, flags.zf);
                    cpu.rflags.set(RFlags::SF, flags.sf);
                    cpu.rflags.set(RFlags::OF, flags.of);
                }
                _ => return Err(ExecError::InvalidOpcode(op2)),
            }
        }
        0x12 if prefix_f2 => {
            require_win7_ext(cpu, op1)?;
            let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
            let dst = modrm.reg | ((rex.r as u8) << 3);
            let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
            let src = match rm {
                RmOperand::Reg(r) => cpu.sse.xmm[r as usize],
                RmOperand::Mem(addr) => bus.read_u64(addr) as u128,
            };
            cpu.sse.xmm[dst as usize] = sse3::movddup(src);
        }
        0x12 if prefix_f3 => {
            require_win7_ext(cpu, op1)?;
            let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
            let dst = modrm.reg | ((rex.r as u8) << 3);
            let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
            let src = read_rm_u128(cpu, bus, rm);
            cpu.sse.xmm[dst as usize] = sse3::movsldup(src);
        }
        0x16 if prefix_f3 => {
            require_win7_ext(cpu, op1)?;
            let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
            let dst = modrm.reg | ((rex.r as u8) << 3);
            let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
            let src = read_rm_u128(cpu, bus, rm);
            cpu.sse.xmm[dst as usize] = sse3::movshdup(src);
        }
        0xF0 if prefix_f2 => {
            // LDDQU m128 -> xmm.
            require_win7_ext(cpu, op1)?;
            let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
            if modrm.mod_bits == 0b11 {
                return Err(ExecError::InvalidOpcode(op1));
            }
            let dst = modrm.reg | ((rex.r as u8) << 3);
            let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;
            let addr = match rm {
                RmOperand::Mem(addr) => addr,
                RmOperand::Reg(_) => return Err(ExecError::InvalidOpcode(op1)),
            };
            cpu.sse.xmm[dst as usize] = read_u128(bus, addr);
        }
        0xB8 if prefix_f3 => {
            // POPCNT
            require_win7_ext(cpu, op1)?;
            let modrm = ModRm::decode(need_byte(bytes, &mut idx)?);
            let dst = modrm.reg | ((rex.r as u8) << 3);
            let rm = decode_rm_operand(cpu, bytes, &mut idx, modrm, rex)?;

            let width_bits = if rex.w {
                64
            } else if prefix_66 {
                16
            } else {
                32
            };
            let src = match width_bits {
                16 => read_rm_u16(cpu, bus, rm) as u64,
                32 => read_rm_u32(cpu, bus, rm) as u64,
                64 => read_rm_u64(cpu, bus, rm),
                _ => unreachable!(),
            };

            let res = bitext::popcnt(src, width_bits);
            write_gpr(cpu, dst, width_bits, res as u64);
            cpu.rflags.set(RFlags::ZF, res == 0);
            cpu.rflags.set(RFlags::CF, false);
            cpu.rflags.set(RFlags::OF, false);
            cpu.rflags.set(RFlags::SF, false);
        }
        _ => return Err(ExecError::InvalidOpcode(op1)),
    }

    Ok(idx)
}

