#![cfg(debug_assertions)]

mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::{discover_block, translate_block, BlockLimits, Tier1Bus};
use aero_types::{Cond, Flag, FlagSet, Gpr, Width};
use aero_x86::tier1::{AluOp, DecodedInst, InstKind, Operand, ShiftOp};
use tier1_common::{
    read_flag, read_gpr, read_gpr_part, write_cpu_to_wasm_bytes, write_flag, write_gpr,
    write_gpr_part, CpuSnapshot, SimpleBus,
};

fn parity_even(byte: u8) -> bool {
    byte.count_ones().is_multiple_of(2)
}

#[derive(Debug, Clone, Copy)]
struct FlagVals {
    cf: bool,
    pf: bool,
    af: bool,
    zf: bool,
    sf: bool,
    of: bool,
}

fn compute_logic_flags(width: Width, result: u64) -> FlagVals {
    let r = width.truncate(result);
    let sign_bit = 1u64 << (width.bits() - 1);
    FlagVals {
        cf: false,
        pf: parity_even(r as u8),
        af: false,
        zf: r == 0,
        sf: (r & sign_bit) != 0,
        of: false,
    }
}

fn compute_add_flags(width: Width, lhs: u64, rhs: u64, result: u64) -> FlagVals {
    let mask = width.mask();
    let lhs = lhs & mask;
    let rhs = rhs & mask;
    let result = result & mask;
    let bits = width.bits();
    let sign_bit = 1u64 << (bits - 1);

    let wide = (lhs as u128) + (rhs as u128);
    let cf = (wide >> bits) != 0;
    let of = ((lhs ^ result) & (rhs ^ result) & sign_bit) != 0;
    let af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    FlagVals {
        cf,
        pf: parity_even(result as u8),
        af,
        zf: result == 0,
        sf: (result & sign_bit) != 0,
        of,
    }
}

fn compute_sub_flags(width: Width, lhs: u64, rhs: u64, result: u64) -> FlagVals {
    let mask = width.mask();
    let lhs = lhs & mask;
    let rhs = rhs & mask;
    let result = result & mask;
    let bits = width.bits();
    let sign_bit = 1u64 << (bits - 1);

    let cf = lhs < rhs;
    let of = ((lhs ^ rhs) & (lhs ^ result) & sign_bit) != 0;
    let af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    FlagVals {
        cf,
        pf: parity_even(result as u8),
        af,
        zf: result == 0,
        sf: (result & sign_bit) != 0,
        of,
    }
}

fn write_flagset(cpu: &mut CpuState, mask: FlagSet, vals: FlagVals) {
    if mask.contains(FlagSet::CF) {
        write_flag(cpu, Flag::Cf, vals.cf);
    }
    if mask.contains(FlagSet::PF) {
        write_flag(cpu, Flag::Pf, vals.pf);
    }
    if mask.contains(FlagSet::AF) {
        write_flag(cpu, Flag::Af, vals.af);
    }
    if mask.contains(FlagSet::ZF) {
        write_flag(cpu, Flag::Zf, vals.zf);
    }
    if mask.contains(FlagSet::SF) {
        write_flag(cpu, Flag::Sf, vals.sf);
    }
    if mask.contains(FlagSet::OF) {
        write_flag(cpu, Flag::Of, vals.of);
    }
}

fn eval_cond(cpu: &CpuState, cond: Cond) -> bool {
    cond.eval(
        read_flag(cpu, Flag::Cf),
        read_flag(cpu, Flag::Pf),
        read_flag(cpu, Flag::Zf),
        read_flag(cpu, Flag::Sf),
        read_flag(cpu, Flag::Of),
    )
}

fn calc_addr(inst: &DecodedInst, cpu: &CpuState, addr: &aero_x86::tier1::Address) -> u64 {
    let mut out = 0u64;
    if addr.rip_relative {
        out = out.wrapping_add(inst.next_rip());
    }
    if let Some(base) = addr.base {
        out = out.wrapping_add(read_gpr(cpu, base));
    }
    if let Some(index) = addr.index {
        out = out.wrapping_add(read_gpr(cpu, index).wrapping_mul(addr.scale as u64));
    }
    out = out.wrapping_add(addr.disp as i64 as u64);
    out
}

fn read_op<B: Tier1Bus>(
    inst: &DecodedInst,
    cpu: &CpuState,
    bus: &B,
    op: &Operand,
    width: Width,
) -> u64 {
    match op {
        Operand::Imm(v) => width.truncate(*v),
        Operand::Reg(r) => read_gpr_part(cpu, r.gpr, width, r.high8),
        Operand::Mem(addr) => bus.read(calc_addr(inst, cpu, addr), width),
    }
}

fn write_op<B: Tier1Bus>(
    inst: &DecodedInst,
    cpu: &mut CpuState,
    bus: &mut B,
    op: &Operand,
    width: Width,
    value: u64,
) {
    let v = width.truncate(value);
    match op {
        Operand::Imm(_) => panic!("cannot write to immediate"),
        Operand::Reg(r) => write_gpr_part(cpu, r.gpr, width, r.high8, v),
        Operand::Mem(addr) => bus.write(calc_addr(inst, cpu, addr), width, v),
    }
}

fn exec_x86_block<B: Tier1Bus>(insts: &[DecodedInst], cpu: &mut CpuState, bus: &mut B) {
    for inst in insts {
        let next = inst.next_rip();
        match &inst.kind {
            InstKind::Nop => {
                cpu.rip = next;
            }
            InstKind::Mov { dst, src, width } => {
                let v = read_op(inst, cpu, bus, src, *width);
                write_op(inst, cpu, bus, dst, *width, v);
                cpu.rip = next;
            }
            InstKind::Lea { dst, addr, width } => {
                let a = calc_addr(inst, cpu, addr);
                write_gpr_part(cpu, dst.gpr, *width, false, a);
                cpu.rip = next;
            }
            InstKind::Alu {
                op,
                dst,
                src,
                width,
            } => {
                let l = read_op(inst, cpu, bus, dst, *width);
                let r = read_op(inst, cpu, bus, src, *width);
                let (res, flags) = match op {
                    AluOp::Add => {
                        let res = width.truncate(l.wrapping_add(r));
                        (res, Some(compute_add_flags(*width, l, r, res)))
                    }
                    AluOp::Sub => {
                        let res = width.truncate(l.wrapping_sub(r));
                        (res, Some(compute_sub_flags(*width, l, r, res)))
                    }
                    AluOp::And => {
                        let res = width.truncate(l & r);
                        (res, Some(compute_logic_flags(*width, res)))
                    }
                    AluOp::Or => {
                        let res = width.truncate(l | r);
                        (res, Some(compute_logic_flags(*width, res)))
                    }
                    AluOp::Xor => {
                        let res = width.truncate(l ^ r);
                        (res, Some(compute_logic_flags(*width, res)))
                    }
                    AluOp::Shl => {
                        // x86 masks shift counts to 5 bits for 8/16/32-bit shifts and 6 bits for
                        // 64-bit shifts.
                        let shift_mask: u32 = if *width == Width::W64 { 63 } else { 31 };
                        let amt = (r as u32) & shift_mask;
                        let res = width.truncate(l << amt);
                        (res, None)
                    }
                    AluOp::Shr => {
                        // x86 masks shift counts to 5 bits for 8/16/32-bit shifts and 6 bits for
                        // 64-bit shifts.
                        let shift_mask: u32 = if *width == Width::W64 { 63 } else { 31 };
                        let amt = (r as u32) & shift_mask;
                        let res = width.truncate(l >> amt);
                        (res, None)
                    }
                    AluOp::Sar => {
                        // x86 masks shift counts to 5 bits for 8/16/32-bit shifts and 6 bits for
                        // 64-bit shifts.
                        let shift_mask: u32 = if *width == Width::W64 { 63 } else { 31 };
                        let amt = (r as u32) & shift_mask;
                        let signed = width.sign_extend(width.truncate(l)) as i64;
                        let res = width.truncate((signed >> amt) as u64);
                        (res, None)
                    }
                };
                if let Some(flags) = flags {
                    write_flagset(cpu, FlagSet::ALU, flags);
                }
                write_op(inst, cpu, bus, dst, *width, res);
                cpu.rip = next;
            }
            InstKind::Shift {
                op,
                dst,
                count,
                width,
            } => {
                let l = read_op(inst, cpu, bus, dst, *width);
                // x86 masks shift counts to 5 bits for 8/16/32-bit shifts and 6 bits for 64-bit
                // shifts (matching the Tier-1 IR interpreter semantics).
                let shift_mask: u32 = if *width == Width::W64 { 63 } else { 31 };
                let amt = (*count as u32) & shift_mask;
                let res = match op {
                    ShiftOp::Shl => width.truncate(l << amt),
                    ShiftOp::Shr => width.truncate(l >> amt),
                    ShiftOp::Sar => {
                        let signed = width.sign_extend(l) as i64;
                        width.truncate((signed >> amt) as u64)
                    }
                };
                // Flags are intentionally left unchanged: Tier-1 translation uses `FlagSet::EMPTY`.
                write_op(inst, cpu, bus, dst, *width, res);
                cpu.rip = next;
            }
            InstKind::Cmp { lhs, rhs, width } => {
                let l = read_op(inst, cpu, bus, lhs, *width);
                let r = read_op(inst, cpu, bus, rhs, *width);
                let res = width.truncate(l.wrapping_sub(r));
                write_flagset(cpu, FlagSet::ALU, compute_sub_flags(*width, l, r, res));
                cpu.rip = next;
            }
            InstKind::Test { lhs, rhs, width } => {
                let l = read_op(inst, cpu, bus, lhs, *width);
                let r = read_op(inst, cpu, bus, rhs, *width);
                let res = width.truncate(l & r);
                write_flagset(cpu, FlagSet::ALU, compute_logic_flags(*width, res));
                cpu.rip = next;
            }
            InstKind::Inc { dst, width } => {
                let l = read_op(inst, cpu, bus, dst, *width);
                let res = width.truncate(l.wrapping_add(1));
                let flags = compute_add_flags(*width, l, 1, res);
                write_flagset(cpu, FlagSet::ALU.without(FlagSet::CF), flags);
                write_op(inst, cpu, bus, dst, *width, res);
                cpu.rip = next;
            }
            InstKind::Dec { dst, width } => {
                let l = read_op(inst, cpu, bus, dst, *width);
                let res = width.truncate(l.wrapping_sub(1));
                let flags = compute_sub_flags(*width, l, 1, res);
                write_flagset(cpu, FlagSet::ALU.without(FlagSet::CF), flags);
                write_op(inst, cpu, bus, dst, *width, res);
                cpu.rip = next;
            }
            InstKind::Push { src } => {
                let v = read_op(inst, cpu, bus, src, Width::W64);
                let rsp = read_gpr(cpu, Gpr::Rsp);
                let new_rsp = rsp.wrapping_sub(8);
                write_gpr(cpu, Gpr::Rsp, new_rsp);
                bus.write(new_rsp, Width::W64, v);
                cpu.rip = next;
            }
            InstKind::Pop { dst } => {
                let rsp = read_gpr(cpu, Gpr::Rsp);
                let v = bus.read(rsp, Width::W64);
                write_gpr(cpu, Gpr::Rsp, rsp.wrapping_add(8));
                write_op(inst, cpu, bus, dst, Width::W64, v);
                cpu.rip = next;
            }
            InstKind::Setcc { cond, dst } => {
                let v = eval_cond(cpu, *cond) as u64;
                write_op(inst, cpu, bus, dst, Width::W8, v);
                cpu.rip = next;
            }
            InstKind::Cmovcc {
                cond,
                dst,
                src,
                width,
            } => {
                if eval_cond(cpu, *cond) {
                    let v = read_op(inst, cpu, bus, src, *width);
                    write_gpr_part(cpu, dst.gpr, *width, false, v);
                }
                cpu.rip = next;
            }
            InstKind::JmpRel { target } => {
                cpu.rip = *target;
                break;
            }
            InstKind::JccRel { cond, target } => {
                cpu.rip = if eval_cond(cpu, *cond) { *target } else { next };
                break;
            }
            InstKind::CallRel { target } => {
                let rsp = read_gpr(cpu, Gpr::Rsp);
                let new_rsp = rsp.wrapping_sub(8);
                write_gpr(cpu, Gpr::Rsp, new_rsp);
                bus.write(new_rsp, Width::W64, next);
                cpu.rip = *target;
                break;
            }
            InstKind::Ret => {
                let rsp = read_gpr(cpu, Gpr::Rsp);
                let target = bus.read(rsp, Width::W64);
                write_gpr(cpu, Gpr::Rsp, rsp.wrapping_add(8));
                cpu.rip = target;
                break;
            }
            InstKind::Invalid => {
                cpu.rip = next;
                break;
            }
        }
    }
}

fn assert_block_ir(
    code: &[u8],
    entry_rip: u64,
    cpu: CpuState,
    mut bus: SimpleBus,
    expected_ir: &str,
) {
    bus.load(entry_rip, code);

    let block = discover_block(&bus, entry_rip, BlockLimits::default());
    let ir = translate_block(&block);
    assert_eq!(ir.to_text(), expected_ir);

    let cpu_initial = cpu;
    let mut cpu_x86 = cpu_initial.clone();
    let mut bus_x86 = bus.clone();
    exec_x86_block(&block.insts, &mut cpu_x86, &mut bus_x86);

    let mut bus_ir = bus;
    let mut cpu_ir_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu_initial, &mut cpu_ir_bytes);
    let _ = aero_jit_x86::tier1::ir::interp::execute_block(&ir, &mut cpu_ir_bytes, &mut bus_ir);

    assert_eq!(
        CpuSnapshot::from_wasm_bytes(&cpu_ir_bytes),
        CpuSnapshot::from_cpu(&cpu_x86),
        "CPU state mismatch\nIR:\n{}\n",
        ir.to_text()
    );
    assert_eq!(bus_ir.mem(), bus_x86.mem(), "memory mismatch");
}

#[test]
fn mov_add_cmp_sete_ret() {
    // mov eax, 5
    // add eax, 7
    // cmp eax, 12
    // sete al
    // ret
    let code = [
        0xb8, 0x05, 0x00, 0x00, 0x00, // mov eax, 5
        0x83, 0xc0, 0x07, // add eax, 7
        0x83, 0xf8, 0x0c, // cmp eax, 12
        0x0f, 0x94, 0xc0, // sete al
        0xc3, // ret
    ];

    let entry = 0x1000u64;
    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x8000);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x8000, Width::W64, 0x2000);

    let expected = "\
block 0x1000:
  v0 = const.i32 0x5
  write.eax v0
  v1 = read.eax
  v2 = const.i32 0x7
  v3 = add.i32 v1, v2 ; flags=CF|PF|AF|ZF|SF|OF
  write.eax v3
  v4 = read.eax
  v5 = const.i32 0xc
  cmpflags.i32 v4, v5 ; flags=CF|PF|AF|ZF|SF|OF
  v6 = evalcond.e
  write.al v6
  v7 = read.rsp
  v8 = load.i64 [v7]
  v9 = const.i64 0x8
  v10 = add.i64 v7, v9
  write.rsp v10
  term jmp [v8]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn call_rel32() {
    // call 0x1010
    let code = [
        0xe8, 0x0b, 0x00, 0x00, 0x00, // call +0x0b (next rip = 0x1005)
    ];
    let entry = 0x1000u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x9000);

    let bus = SimpleBus::new(0x10000);

    let expected = "\
block 0x1000:
  v0 = read.rsp
  v1 = const.i64 0x8
  v2 = sub.i64 v0, v1
  write.rsp v2
  v3 = const.i64 0x1005
  store.i64 [v2], v3
  term jmp 0x1010
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn cmp_jne_not_taken() {
    // mov eax, 0
    // cmp eax, 0
    // jne +5
    let code = [
        0xb8, 0x00, 0x00, 0x00, 0x00, // mov eax, 0
        0x83, 0xf8, 0x00, // cmp eax, 0
        0x75, 0x05, // jne +5
    ];
    let entry = 0x3000u64;

    let cpu = CpuState {
        rip: entry,
        ..Default::default()
    };

    let bus = SimpleBus::new(0x10000);

    let expected = "\
block 0x3000:
  v0 = const.i32 0x0
  write.eax v0
  v1 = read.eax
  v2 = const.i32 0x0
  cmpflags.i32 v1, v2 ; flags=CF|PF|AF|ZF|SF|OF
  v3 = evalcond.ne
  term jcc v3, 0x300f, 0x300a
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn lea_sib_ret() {
    // lea rax, [rcx + rdx*4 + 0x10]
    // ret
    let code = [
        0x48, 0x8d, 0x44, 0x91, 0x10, // lea rax, [rcx + rdx*4 + 0x10]
        0xc3, // ret
    ];
    let entry = 0x4000u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x8800);
    write_gpr(&mut cpu, Gpr::Rcx, 0x100);
    write_gpr(&mut cpu, Gpr::Rdx, 0x2);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x8800, Width::W64, 0x5000);

    let expected = "\
block 0x4000:
  v0 = read.rcx
  v1 = read.rdx
  v2 = const.i64 0x2
  v3 = shl.i64 v1, v2
  v4 = add.i64 v0, v3
  v5 = const.i64 0x10
  v6 = add.i64 v4, v5
  write.rax v6
  v7 = read.rsp
  v8 = load.i64 [v7]
  v9 = const.i64 0x8
  v10 = add.i64 v7, v9
  write.rsp v10
  term jmp [v8]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn group2_shr_shl_imm1_and_imm8() {
    // mov rbx, 0x10000
    // shr rbx, 13   (C1 /5 ib)
    // shl rbx, 1    (D1 /4)
    // ret
    let code = [
        0x48, 0xbb, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // mov rbx, 0x10000
        0x48, 0xc1, 0xeb, 0x0d, // shr rbx, 13
        0x48, 0xd1, 0xe3, // shl rbx, 1
        0xc3, // ret
    ];

    let entry = 0x6000u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x9000);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x9000, Width::W64, 0x7000);

    let expected = "\
block 0x6000:
  v0 = const.i64 0x10000
  write.rbx v0
  v1 = read.rbx
  v2 = const.i64 0xd
  v3 = shr.i64 v1, v2
  write.rbx v3
  v4 = read.rbx
  v5 = const.i64 0x1
  v6 = shl.i64 v4, v5
  write.rbx v6
  v7 = read.rsp
  v8 = load.i64 [v7]
  v9 = const.i64 0x8
  v10 = add.i64 v7, v9
  write.rsp v10
  term jmp [v8]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn group2_shl_ax_imm8_masks_count_like_x86() {
    // mov ax, 1
    // shl ax, 17  (C1 /4 ib, operand-size override)
    // ret
    //
    // x86 masks shift counts to 5 bits for 16-bit operands, so 17 is *not* reduced to 1.
    // This is an important behavior difference from masking by `(bits - 1)`.
    let code = [
        0x66, 0xb8, 0x01, 0x00, // mov ax, 1
        0x66, 0xc1, 0xe0, 0x11, // shl ax, 17
        0xc3, // ret
    ];

    let entry = 0x6100u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x9000);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x9000, Width::W64, 0x7000);

    let expected = "\
block 0x6100:
  v0 = const.i16 0x1
  write.ax v0
  v1 = read.ax
  v2 = const.i16 0x11
  v3 = shl.i16 v1, v2
  write.ax v3
  v4 = read.rsp
  v5 = load.i64 [v4]
  v6 = const.i64 0x8
  v7 = add.i64 v4, v6
  write.rsp v7
  term jmp [v5]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn group2_shl_al_imm8_masks_count_like_x86() {
    // mov al, 1
    // shl al, 9  (C0 /4 ib)
    // ret
    //
    // x86 masks 8-bit shift counts to 5 bits, so 9 is *not* reduced to 1.
    let code = [
        0xb0, 0x01, // mov al, 1
        0xc0, 0xe0, 0x09, // shl al, 9
        0xc3, // ret
    ];

    let entry = 0x6200u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x9000);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x9000, Width::W64, 0x7000);

    let expected = "\
block 0x6200:
  v0 = const.i8 0x1
  write.al v0
  v1 = read.al
  v2 = const.i8 0x9
  v3 = shl.i8 v1, v2
  write.al v3
  v4 = read.rsp
  v5 = load.i64 [v4]
  v6 = const.i64 0x8
  v7 = add.i64 v4, v6
  write.rsp v7
  term jmp [v5]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn group2_shl_eax_imm1_zero_extends_to_64() {
    // mov rax, 0xffff_ffff_0000_0001
    // shl eax, 1  (D1 /4, 32-bit op => zero-extends into rax)
    // ret
    let code = [
        0x48, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0xff, // mov rax, 0xffffffff00000001
        0xd1, 0xe0, // shl eax, 1
        0xc3, // ret
    ];

    let entry = 0x6300u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x9000);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x9000, Width::W64, 0x7000);

    let expected = "\
block 0x6300:
  v0 = const.i64 0xffffffff00000001
  write.rax v0
  v1 = read.eax
  v2 = const.i32 0x1
  v3 = shl.i32 v1, v2
  write.eax v3
  v4 = read.rsp
  v5 = load.i64 [v4]
  v6 = const.i64 0x8
  v7 = add.i64 v4, v6
  write.rsp v7
  term jmp [v5]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn group2_sar_al_imm1_decodes_d0() {
    // mov al, 0x80
    // sar al, 1  (D0 /7)
    // ret
    let code = [
        0xb0, 0x80, // mov al, 0x80
        0xd0, 0xf8, // sar al, 1
        0xc3, // ret
    ];

    let entry = 0x6400u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x9000);
    // Ensure 8-bit writes preserve the rest of the 64-bit GPR.
    write_gpr(&mut cpu, Gpr::Rax, 0x1122_3344_5566_7788);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x9000, Width::W64, 0x7000);

    let expected = "\
block 0x6400:
  v0 = const.i8 0x80
  write.al v0
  v1 = read.al
  v2 = const.i8 0x1
  v3 = sar.i8 v1, v2
  write.al v3
  v4 = read.rsp
  v5 = load.i64 [v4]
  v6 = const.i64 0x8
  v7 = add.i64 v4, v6
  write.rsp v7
  term jmp [v5]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}

#[test]
fn group2_shl_ah_imm1_decodes_high8_reg() {
    // mov ax, 0x0100   (AH=1, AL=0)
    // shl ah, 1        (D0 /4, high-8 register encoding without REX)
    // ret
    let code = [
        0x66, 0xb8, 0x00, 0x01, // mov ax, 0x0100
        0xd0, 0xe4, // shl ah, 1
        0xc3, // ret
    ];

    let entry = 0x6500u64;

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    write_gpr(&mut cpu, Gpr::Rsp, 0x9000);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x9000, Width::W64, 0x7000);

    let expected = "\
block 0x6500:
  v0 = const.i16 0x100
  write.ax v0
  v1 = read.ah
  v2 = const.i8 0x1
  v3 = shl.i8 v1, v2
  write.ah v3
  v4 = read.rsp
  v5 = load.i64 [v4]
  v6 = const.i64 0x8
  v7 = add.i64 v4, v6
  write.rsp v7
  term jmp [v5]
";

    assert_block_ir(&code, entry, cpu, bus, expected);
}
