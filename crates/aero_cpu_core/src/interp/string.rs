use crate::bus::Bus;
use crate::cpu::{Cpu, CpuMode, RFlags, Segment};
use crate::interp::decode::PrefixState;
use crate::interp::ExecError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepPrefix {
    #[default]
    None,
    F2,
    F3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepMode {
    None,
    Rep,
    Repe,
    Repne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrSize {
    A16,
    A32,
    A64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringOp {
    Movs,
    Stos,
    Lods,
    Cmps,
    Scas,
}

#[derive(Clone, Debug)]
pub struct DecodedStringInst {
    pub op: StringOp,
    pub elem_size: usize,
    pub prefixes: PrefixState,
}

impl DecodedStringInst {
    pub fn new(op: StringOp, elem_size: usize, prefixes: PrefixState) -> Self {
        Self {
            op,
            elem_size,
            prefixes,
        }
    }
}

pub fn exec_string<B: Bus>(cpu: &mut Cpu, bus: &mut B, inst: &DecodedStringInst) -> Result<(), ExecError> {
    let addr_size = effective_addr_size(cpu.mode, &inst.prefixes);
    let rep_mode = effective_rep_mode(inst.op, inst.prefixes.rep);

    match inst.op {
        StringOp::Movs => exec_movs(cpu, bus, inst, addr_size, rep_mode),
        StringOp::Stos => exec_stos(cpu, bus, inst, addr_size, rep_mode),
        StringOp::Lods => exec_lods(cpu, bus, inst, addr_size, rep_mode),
        StringOp::Cmps => exec_cmps(cpu, bus, inst, addr_size, rep_mode),
        StringOp::Scas => exec_scas(cpu, bus, inst, addr_size, rep_mode),
    }
}

fn effective_addr_size(mode: CpuMode, p: &PrefixState) -> AddrSize {
    match mode {
        CpuMode::Real16 => {
            if p.address_size_override {
                AddrSize::A32
            } else {
                AddrSize::A16
            }
        }
        CpuMode::Protected32 => {
            if p.address_size_override {
                AddrSize::A16
            } else {
                AddrSize::A32
            }
        }
        CpuMode::Long64 => {
            if p.address_size_override {
                AddrSize::A32
            } else {
                AddrSize::A64
            }
        }
    }
}

fn effective_rep_mode(op: StringOp, rep: RepPrefix) -> RepMode {
    match rep {
        RepPrefix::None => RepMode::None,
        RepPrefix::F3 => match op {
            StringOp::Cmps | StringOp::Scas => RepMode::Repe,
            _ => RepMode::Rep,
        },
        RepPrefix::F2 => match op {
            StringOp::Cmps | StringOp::Scas => RepMode::Repne,
            _ => RepMode::Rep,
        },
    }
}

fn read_count(cpu: &Cpu, addr_size: AddrSize) -> u64 {
    match addr_size {
        AddrSize::A16 => cpu.regs.cx() as u64,
        AddrSize::A32 => cpu.regs.ecx() as u64,
        AddrSize::A64 => cpu.regs.rcx,
    }
}

fn write_count(cpu: &mut Cpu, addr_size: AddrSize, value: u64) {
    match addr_size {
        AddrSize::A16 => cpu.regs.set_cx(value as u16),
        AddrSize::A32 => cpu.regs.set_ecx(value as u32, cpu.mode),
        AddrSize::A64 => cpu.regs.set_rcx(value),
    }
}

fn read_si(cpu: &Cpu, addr_size: AddrSize) -> u64 {
    match addr_size {
        AddrSize::A16 => cpu.regs.si() as u64,
        AddrSize::A32 => cpu.regs.esi() as u64,
        AddrSize::A64 => cpu.regs.rsi,
    }
}

fn write_si(cpu: &mut Cpu, addr_size: AddrSize, value: u64) {
    match addr_size {
        AddrSize::A16 => cpu.regs.set_si(value as u16),
        AddrSize::A32 => cpu.regs.set_esi(value as u32, cpu.mode),
        AddrSize::A64 => cpu.regs.set_rsi(value),
    }
}

fn read_di(cpu: &Cpu, addr_size: AddrSize) -> u64 {
    match addr_size {
        AddrSize::A16 => cpu.regs.di() as u64,
        AddrSize::A32 => cpu.regs.edi() as u64,
        AddrSize::A64 => cpu.regs.rdi,
    }
}

fn write_di(cpu: &mut Cpu, addr_size: AddrSize, value: u64) {
    match addr_size {
        AddrSize::A16 => cpu.regs.set_di(value as u16),
        AddrSize::A32 => cpu.regs.set_edi(value as u32, cpu.mode),
        AddrSize::A64 => cpu.regs.set_rdi(value),
    }
}

fn step_delta(cpu: &Cpu, elem_size: usize) -> i64 {
    if cpu.rflags.df() {
        -(elem_size as i64)
    } else {
        elem_size as i64
    }
}

fn add_wrapping(value: u64, delta: i64, addr_size: AddrSize) -> u64 {
    match addr_size {
        AddrSize::A16 => {
            let v = value as u16;
            let d = delta as i32;
            v.wrapping_add(d as u16) as u64
        }
        AddrSize::A32 => {
            let v = value as u32;
            let d = delta as i64;
            v.wrapping_add(d as u32) as u64
        }
        AddrSize::A64 => value.wrapping_add(delta as u64),
    }
}

fn advance_n(value: u64, elem_size: usize, count: u64, df: bool, addr_size: AddrSize) -> u64 {
    let total = (elem_size as u64).wrapping_mul(count);
    match addr_size {
        AddrSize::A16 => {
            let v = value as u16;
            let t = total as u16;
            if df {
                v.wrapping_sub(t) as u64
            } else {
                v.wrapping_add(t) as u64
            }
        }
        AddrSize::A32 => {
            let v = value as u32;
            let t = total as u32;
            if df {
                v.wrapping_sub(t) as u64
            } else {
                v.wrapping_add(t) as u64
            }
        }
        AddrSize::A64 => {
            if df {
                value.wrapping_sub(total)
            } else {
                value.wrapping_add(total)
            }
        }
    }
}

fn src_segment(inst: &DecodedStringInst) -> Segment {
    // Only MOVS/CMPS/LODS consult the segment override, and it applies to the source.
    inst.prefixes.segment_override.unwrap_or(Segment::Ds)
}

fn linear(cpu: &Cpu, seg: Segment, offset: u64) -> u64 {
    cpu.seg_base(seg).wrapping_add(offset)
}

fn read_mem<B: Bus>(bus: &mut B, addr: u64, size: usize) -> u64 {
    match size {
        1 => bus.read_u8(addr) as u64,
        2 => bus.read_u16(addr) as u64,
        4 => bus.read_u32(addr) as u64,
        8 => bus.read_u64(addr),
        _ => panic!("unsupported element size: {size}"),
    }
}

fn write_mem<B: Bus>(bus: &mut B, addr: u64, size: usize, value: u64) {
    match size {
        1 => bus.write_u8(addr, value as u8),
        2 => bus.write_u16(addr, value as u16),
        4 => bus.write_u32(addr, value as u32),
        8 => bus.write_u64(addr, value),
        _ => panic!("unsupported element size: {size}"),
    }
}

fn stos_pattern(cpu: &Cpu, size: usize) -> [u8; 8] {
    let value = match size {
        1 => cpu.regs.al() as u64,
        2 => cpu.regs.ax() as u64,
        4 => cpu.regs.eax() as u64,
        8 => cpu.regs.rax,
        _ => panic!("unsupported element size: {size}"),
    };
    value.to_le_bytes()
}

const BULK_THRESHOLD_BYTES: usize = 64;

fn exec_movs<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedStringInst,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), ExecError> {
    let elem_size = inst.elem_size;
    let delta = step_delta(cpu, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(cpu, addr_size),
    };

    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    let src_seg = src_segment(inst);

    // REP MOVS* fast path.
    if rep_mode != RepMode::None && bus.supports_bulk_copy() {
        let total_bytes = (count as usize).saturating_mul(elem_size);
        if total_bytes >= BULK_THRESHOLD_BYTES {
            let si = read_si(cpu, addr_size);
            let di = read_di(cpu, addr_size);

            let src0 = linear(cpu, src_seg, si);
            let dst0 = linear(cpu, Segment::Es, di);

            let (src_start, dst_start) = if cpu.rflags.df() {
                // `si`/`di` point at the last element; bulk op wants the lowest address.
                let back = (count - 1).wrapping_mul(elem_size as u64);
                (src0.wrapping_sub(back), dst0.wrapping_sub(back))
            } else {
                (src0, dst0)
            };

            let len = total_bytes as u64;
            let src_end = src_start.wrapping_add(len);
            let dst_end = dst_start.wrapping_add(len);

            let overlap = src_start < dst_end && dst_start < src_end;
            let hazard = if !overlap {
                false
            } else if !cpu.rflags.df() {
                // DF=0 copies low->high. Hazard when destination starts inside source at a higher address.
                src_start < dst_start && dst_start < src_end
            } else {
                // DF=1 copies high->low. Hazard when source starts inside destination at a higher address.
                dst_start < src_start && src_start < dst_end
            };

            if !hazard && bus.bulk_copy(dst_start, src_start, total_bytes) {
                let si_new = advance_n(si, elem_size, count, cpu.rflags.df(), addr_size);
                let di_new = advance_n(di, elem_size, count, cpu.rflags.df(), addr_size);
                write_si(cpu, addr_size, si_new);
                write_di(cpu, addr_size, di_new);
                write_count(cpu, addr_size, 0);
                return Ok(());
            }
        }
    }

    while count != 0 {
        let si = read_si(cpu, addr_size);
        let di = read_di(cpu, addr_size);
        let src_addr = linear(cpu, src_seg, si);
        let dst_addr = linear(cpu, Segment::Es, di);

        let value = read_mem(bus, src_addr, elem_size);
        write_mem(bus, dst_addr, elem_size, value);

        let si_new = add_wrapping(si, delta, addr_size);
        let di_new = add_wrapping(di, delta, addr_size);
        write_si(cpu, addr_size, si_new);
        write_di(cpu, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(cpu, addr_size, count);
        } else {
            break;
        }
    }

    Ok(())
}

fn exec_stos<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedStringInst,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), ExecError> {
    let elem_size = inst.elem_size;
    let delta = step_delta(cpu, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(cpu, addr_size),
    };

    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    // REP STOS* fast path.
    if rep_mode != RepMode::None && bus.supports_bulk_set() {
        let total_bytes = (count as usize).saturating_mul(elem_size);
        if total_bytes >= BULK_THRESHOLD_BYTES {
            let di = read_di(cpu, addr_size);
            let dst0 = linear(cpu, Segment::Es, di);
            let dst_start = if cpu.rflags.df() {
                let back = (count - 1).wrapping_mul(elem_size as u64);
                dst0.wrapping_sub(back)
            } else {
                dst0
            };

            let pattern = stos_pattern(cpu, elem_size);
            if bus.bulk_set(dst_start, &pattern[..elem_size], count as usize) {
                let di_new = advance_n(di, elem_size, count, cpu.rflags.df(), addr_size);
                write_di(cpu, addr_size, di_new);
                write_count(cpu, addr_size, 0);
                return Ok(());
            }
        }
    }

    while count != 0 {
        let di = read_di(cpu, addr_size);
        let dst_addr = linear(cpu, Segment::Es, di);

        let value = match elem_size {
            1 => cpu.regs.al() as u64,
            2 => cpu.regs.ax() as u64,
            4 => cpu.regs.eax() as u64,
            8 => cpu.regs.rax,
            _ => unreachable!(),
        };
        write_mem(bus, dst_addr, elem_size, value);

        let di_new = add_wrapping(di, delta, addr_size);
        write_di(cpu, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(cpu, addr_size, count);
        } else {
            break;
        }
    }

    Ok(())
}

fn exec_lods<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedStringInst,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), ExecError> {
    let elem_size = inst.elem_size;
    let delta = step_delta(cpu, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(cpu, addr_size),
    };

    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    let src_seg = src_segment(inst);

    while count != 0 {
        let si = read_si(cpu, addr_size);
        let src_addr = linear(cpu, src_seg, si);
        let value = read_mem(bus, src_addr, elem_size);

        match elem_size {
            1 => cpu.regs.set_al(value as u8),
            2 => cpu.regs.set_ax(value as u16),
            4 => cpu.regs.set_eax(value as u32, cpu.mode),
            8 => cpu.regs.set_rax(value),
            _ => unreachable!(),
        }

        let si_new = add_wrapping(si, delta, addr_size);
        write_si(cpu, addr_size, si_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(cpu, addr_size, count);
        } else {
            break;
        }
    }

    Ok(())
}

fn update_sub_flags(cpu: &mut Cpu, dest: u64, src: u64, size: usize) {
    let bits = (size * 8) as u32;
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let dest = dest & mask;
    let src = src & mask;
    let result = dest.wrapping_sub(src) & mask;

    let sign_bit = 1u64 << (bits - 1);
    cpu.rflags.set(RFlags::CF, dest < src);
    cpu.rflags.set(RFlags::ZF, result == 0);
    cpu.rflags.set(RFlags::SF, (result & sign_bit) != 0);
    cpu.rflags
        .set(RFlags::OF, ((dest ^ src) & (dest ^ result) & sign_bit) != 0);
    cpu.rflags
        .set(RFlags::AF, ((dest ^ src ^ result) & 0x10) != 0);

    let low = (result & 0xFF) as u8;
    cpu.rflags
        .set(RFlags::PF, low.count_ones() % 2 == 0);
}

fn exec_cmps<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedStringInst,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), ExecError> {
    let elem_size = inst.elem_size;
    let delta = step_delta(cpu, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(cpu, addr_size),
    };

    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    let src_seg = src_segment(inst);

    while count != 0 {
        let si = read_si(cpu, addr_size);
        let di = read_di(cpu, addr_size);
        let src_addr = linear(cpu, src_seg, si);
        let dst_addr = linear(cpu, Segment::Es, di);

        let src_val = read_mem(bus, src_addr, elem_size);
        let dst_val = read_mem(bus, dst_addr, elem_size);
        update_sub_flags(cpu, dst_val, src_val, elem_size);

        let si_new = add_wrapping(si, delta, addr_size);
        let di_new = add_wrapping(di, delta, addr_size);
        write_si(cpu, addr_size, si_new);
        write_di(cpu, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(cpu, addr_size, count);
            match rep_mode {
                RepMode::Rep => {}
                RepMode::Repe => {
                    if !cpu.rflags.zf() {
                        break;
                    }
                }
                RepMode::Repne => {
                    if cpu.rflags.zf() {
                        break;
                    }
                }
                RepMode::None => unreachable!(),
            }
        } else {
            break;
        }
    }

    Ok(())
}

fn exec_scas<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedStringInst,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), ExecError> {
    let elem_size = inst.elem_size;
    let delta = step_delta(cpu, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(cpu, addr_size),
    };

    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    while count != 0 {
        let di = read_di(cpu, addr_size);
        let mem_addr = linear(cpu, Segment::Es, di);
        let mem_val = read_mem(bus, mem_addr, elem_size);

        let acc_val = match elem_size {
            1 => cpu.regs.al() as u64,
            2 => cpu.regs.ax() as u64,
            4 => cpu.regs.eax() as u64,
            8 => cpu.regs.rax,
            _ => unreachable!(),
        };
        update_sub_flags(cpu, acc_val, mem_val, elem_size);

        let di_new = add_wrapping(di, delta, addr_size);
        write_di(cpu, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(cpu, addr_size, count);
            match rep_mode {
                RepMode::Rep => {}
                RepMode::Repe => {
                    if !cpu.rflags.zf() {
                        break;
                    }
                }
                RepMode::Repne => {
                    if cpu.rflags.zf() {
                        break;
                    }
                }
                RepMode::None => unreachable!(),
            }
        } else {
            break;
        }
    }

    Ok(())
}
