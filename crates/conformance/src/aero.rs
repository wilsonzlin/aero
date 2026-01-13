use crate::corpus::{TemplateKind, TestCase};
use crate::{
    CpuState, ExecOutcome, Fault, FLAG_AF, FLAG_CF, FLAG_FIXED_1, FLAG_OF, FLAG_PF, FLAG_SF,
    FLAG_ZF,
};

pub struct AeroBackend {
    mem_fault_signal: i32,
}

impl AeroBackend {
    pub fn new(mem_fault_signal: i32) -> Self {
        Self { mem_fault_signal }
    }

    pub fn execute(&mut self, case: &TestCase) -> ExecOutcome {
        let mut state = case.init;
        let mut memory = case.memory.clone();

        match execute_one(
            case.template.kind,
            &mut state,
            &mut memory,
            case.mem_base,
            case.template.bytes.len(),
            self.mem_fault_signal,
        ) {
            Ok(()) => ExecOutcome {
                state,
                memory,
                fault: None,
            },
            Err(fault) => ExecOutcome {
                state,
                memory,
                fault: Some(fault),
            },
        }
    }
}

fn execute_one(
    kind: TemplateKind,
    state: &mut CpuState,
    memory: &mut [u8],
    mem_base: u64,
    instr_len: usize,
    mem_fault_signal: i32,
) -> Result<(), Fault> {
    match kind {
        TemplateKind::MovRaxRbx => {
            state.rax = state.rbx;
        }
        TemplateKind::MovRbxRax => {
            state.rbx = state.rax;
        }
        TemplateKind::AddRaxRbx => {
            let a = state.rax;
            let b = state.rbx;
            let result = a.wrapping_add(b);
            state.rax = result;
            update_flags_add(state, a, b, result);
        }
        TemplateKind::SubRaxRbx => {
            let a = state.rax;
            let b = state.rbx;
            let result = a.wrapping_sub(b);
            state.rax = result;
            update_flags_sub(state, a, b, result);
        }
        TemplateKind::AdcRaxRbx => {
            let a = state.rax;
            let b = state.rbx;
            let carry = if (state.rflags & FLAG_CF) != 0 { 1u64 } else { 0u64 };
            let result = a.wrapping_add(b).wrapping_add(carry);
            state.rax = result;
            update_flags_adc(state, a, b, carry, result);
        }
        TemplateKind::SbbRaxRbx => {
            let a = state.rax;
            let b = state.rbx;
            let borrow = if (state.rflags & FLAG_CF) != 0 { 1u64 } else { 0u64 };
            let result = a.wrapping_sub(b).wrapping_sub(borrow);
            state.rax = result;
            update_flags_sbb(state, a, b, borrow, result);
        }
        TemplateKind::XorRaxRbx => {
            let result = state.rax ^ state.rbx;
            state.rax = result;
            update_flags_logic(state, result);
        }
        TemplateKind::AndRaxRbx => {
            let result = state.rax & state.rbx;
            state.rax = result;
            update_flags_logic(state, result);
        }
        TemplateKind::OrRaxRbx => {
            let result = state.rax | state.rbx;
            state.rax = result;
            update_flags_logic(state, result);
        }
        TemplateKind::CmpRaxRbx => {
            let a = state.rax;
            let b = state.rbx;
            let result = a.wrapping_sub(b);
            update_flags_sub(state, a, b, result);
        }
        TemplateKind::CmpRaxImm32 => {
            let a = state.rax;
            let b = 0x8000_0000u64.wrapping_neg(); // sign-extended imm32 0x80000000
            let result = a.wrapping_sub(b);
            update_flags_sub(state, a, b, result);
        }
        TemplateKind::IncRax => {
            let a = state.rax;
            let result = a.wrapping_add(1);
            state.rax = result;
            update_flags_inc(state, a, result);
        }
        TemplateKind::DecRax => {
            let a = state.rax;
            let result = a.wrapping_sub(1);
            state.rax = result;
            update_flags_dec(state, a, result);
        }
        TemplateKind::ShlRax1 => {
            exec_shl_u64(state, 1);
        }
        TemplateKind::ShrRax1 => {
            exec_shr_u64(state, 1);
        }
        TemplateKind::SarRax1 => {
            exec_sar_u64(state, 1);
        }
        TemplateKind::ShlRaxCl => {
            exec_shl_u64(state, (state.rcx as u8) as u32);
        }
        TemplateKind::ShrRaxCl => {
            exec_shr_u64(state, (state.rcx as u8) as u32);
        }
        TemplateKind::SarRaxCl => {
            exec_sar_u64(state, (state.rcx as u8) as u32);
        }
        TemplateKind::RolRax1 => {
            exec_rol_u64(state, 1);
        }
        TemplateKind::RorRax1 => {
            exec_ror_u64(state, 1);
        }
        TemplateKind::RolRaxCl => {
            exec_rol_u64(state, (state.rcx as u8) as u32);
        }
        TemplateKind::RorRaxCl => {
            exec_ror_u64(state, (state.rcx as u8) as u32);
        }
        TemplateKind::MulRbx => {
            let a = state.rax as u128;
            let b = state.rbx as u128;
            let product = a * b;
            state.rax = product as u64;
            state.rdx = (product >> 64) as u64;
            update_flags_mul(state, state.rdx != 0);
        }
        TemplateKind::ImulRbx => {
            let a = state.rax as i64 as i128;
            let b = state.rbx as i64 as i128;
            let product = a * b;
            state.rax = product as u64;
            state.rdx = (product >> 64) as u64;
            let low = state.rax as i64;
            let overflow = product != low as i128;
            update_flags_mul(state, overflow);
        }
        TemplateKind::ImulRaxRbx => {
            let a = state.rax as i64 as i128;
            let b = state.rbx as i64 as i128;
            let product = a * b;
            let low = product as i64;
            state.rax = low as u64;
            let overflow = product != low as i128;
            update_flags_mul(state, overflow);
        }
        TemplateKind::ImulRaxRbxImm8 => {
            let imm = 7i8 as i64 as i128;
            let src = state.rbx as i64 as i128;
            let product = src * imm;
            let low = product as i64;
            state.rax = low as u64;
            let overflow = product != low as i128;
            update_flags_mul(state, overflow);
        }
        TemplateKind::LeaRaxBaseIndexScaleDisp => {
            state.rax = state
                .rbx
                .wrapping_add(state.rcx.wrapping_mul(4))
                .wrapping_add(0x10);
        }
        TemplateKind::XchgRaxRbx => {
            core::mem::swap(&mut state.rax, &mut state.rbx);
        }
        TemplateKind::BsfRaxRbx => {
            exec_bsf_u64(state);
        }
        TemplateKind::BsrRaxRbx => {
            exec_bsr_u64(state);
        }
        TemplateKind::AddEaxEbx => {
            let a = state.rax as u32;
            let b = state.rbx as u32;
            let result = a.wrapping_add(b);
            state.rax = result as u64;
            update_flags_add32(state, a, b, result);
        }
        TemplateKind::SubEaxEbx => {
            let a = state.rax as u32;
            let b = state.rbx as u32;
            let result = a.wrapping_sub(b);
            state.rax = result as u64;
            update_flags_sub32(state, a, b, result);
        }
        TemplateKind::AdcEaxEbx => {
            let a = state.rax as u32;
            let b = state.rbx as u32;
            let carry = if (state.rflags & FLAG_CF) != 0 { 1u32 } else { 0u32 };
            let result = a.wrapping_add(b).wrapping_add(carry);
            state.rax = result as u64;
            update_flags_adc32(state, a, b, carry, result);
        }
        TemplateKind::SbbEaxEbx => {
            let a = state.rax as u32;
            let b = state.rbx as u32;
            let borrow = if (state.rflags & FLAG_CF) != 0 { 1u32 } else { 0u32 };
            let result = a.wrapping_sub(b).wrapping_sub(borrow);
            state.rax = result as u64;
            update_flags_sbb32(state, a, b, borrow, result);
        }
        TemplateKind::XorEaxEbx => {
            let result = (state.rax as u32) ^ (state.rbx as u32);
            state.rax = result as u64;
            update_flags_logic32(state, result);
        }
        TemplateKind::CmpEaxEbx => {
            let a = state.rax as u32;
            let b = state.rbx as u32;
            let result = a.wrapping_sub(b);
            update_flags_sub32(state, a, b, result);
        }
        TemplateKind::MovM64Rax => {
            write_u64(memory, mem_base, state.rdi, state.rax, mem_fault_signal)?;
        }
        TemplateKind::MovRaxM64 => {
            state.rax = read_u64(memory, mem_base, state.rdi, mem_fault_signal)?;
        }
        TemplateKind::AddM64Rax => {
            let a = read_u64(memory, mem_base, state.rdi, mem_fault_signal)?;
            let b = state.rax;
            let result = a.wrapping_add(b);
            write_u64(memory, mem_base, state.rdi, result, mem_fault_signal)?;
            update_flags_add(state, a, b, result);
        }
        TemplateKind::SubM64Rax => {
            let a = read_u64(memory, mem_base, state.rdi, mem_fault_signal)?;
            let b = state.rax;
            let result = a.wrapping_sub(b);
            write_u64(memory, mem_base, state.rdi, result, mem_fault_signal)?;
            update_flags_sub(state, a, b, result);
        }
        TemplateKind::Ud2 => {
            return Err(Fault::Signal(libc::SIGILL));
        }
        TemplateKind::MovRaxM64Abs0 => {
            // Force a fault when dereferencing address 0 in user-mode.
            state.rax = read_u64(memory, mem_base, 0, mem_fault_signal)?;
        }
    }

    state.rip = state.rip.wrapping_add(instr_len as u64);
    state.rflags |= FLAG_FIXED_1;
    Ok(())
}

fn read_u64(memory: &[u8], base: u64, addr: u64, mem_fault_signal: i32) -> Result<u64, Fault> {
    let fault = Fault::Signal(mem_fault_signal);
    let offset = addr.checked_sub(base).ok_or(fault)? as usize;
    let end = offset.checked_add(8).ok_or(fault)?;
    if end > memory.len() {
        return Err(fault);
    }
    let bytes: [u8; 8] = memory[offset..end]
        .try_into()
        .expect("slice length checked");
    Ok(u64::from_le_bytes(bytes))
}

fn write_u64(
    memory: &mut [u8],
    base: u64,
    addr: u64,
    value: u64,
    mem_fault_signal: i32,
) -> Result<(), Fault> {
    let fault = Fault::Signal(mem_fault_signal);
    let offset = addr.checked_sub(base).ok_or(fault)? as usize;
    let end = offset.checked_add(8).ok_or(fault)?;
    if end > memory.len() {
        return Err(fault);
    }
    memory[offset..end].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn update_flags_add(state: &mut CpuState, a: u64, b: u64, result: u64) {
    let cf = result < a;
    let of = (((a ^ result) & (b ^ result)) & 0x8000_0000_0000_0000) != 0;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    update_flags_arith(state, result, cf, of, af);
}

fn update_flags_sub(state: &mut CpuState, a: u64, b: u64, result: u64) {
    let cf = a < b;
    let of = (((a ^ b) & (a ^ result)) & 0x8000_0000_0000_0000) != 0;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    update_flags_arith(state, result, cf, of, af);
}

fn update_flags_inc(state: &mut CpuState, a: u64, result: u64) {
    let cf = (state.rflags & FLAG_CF) != 0;
    let of = a == 0x7FFF_FFFF_FFFF_FFFF;
    let af = ((a ^ 1 ^ result) & 0x10) != 0;
    update_flags_arith(state, result, cf, of, af);
}

fn update_flags_dec(state: &mut CpuState, a: u64, result: u64) {
    let cf = (state.rflags & FLAG_CF) != 0;
    let of = a == 0x8000_0000_0000_0000;
    let af = ((a ^ 1 ^ result) & 0x10) != 0;
    update_flags_arith(state, result, cf, of, af);
}

fn update_flags_adc(state: &mut CpuState, a: u64, b: u64, carry: u64, result: u64) {
    let sum = (a as u128) + (b as u128) + (carry as u128);
    let cf = sum > u64::MAX as u128;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    let sum_i = (a as i64 as i128) + (b as i64 as i128) + (carry as i128);
    let of = sum_i < i64::MIN as i128 || sum_i > i64::MAX as i128;
    update_flags_arith(state, result, cf, of, af);
}

fn update_flags_sbb(state: &mut CpuState, a: u64, b: u64, borrow: u64, result: u64) {
    let subtrahend = (b as u128) + (borrow as u128);
    let cf = (a as u128) < subtrahend;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    let diff_i = (a as i64 as i128) - (b as i64 as i128) - (borrow as i128);
    let of = diff_i < i64::MIN as i128 || diff_i > i64::MAX as i128;
    update_flags_arith(state, result, cf, of, af);
}

fn update_flags_logic(state: &mut CpuState, result: u64) {
    let mut flags = state.rflags;
    flags &= !(FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF | FLAG_OF);
    flags |= FLAG_FIXED_1;

    if parity_even(result as u8) {
        flags |= FLAG_PF;
    }
    if result == 0 {
        flags |= FLAG_ZF;
    }
    if (result & 0x8000_0000_0000_0000) != 0 {
        flags |= FLAG_SF;
    }

    state.rflags = flags;
}

fn update_flags_add32(state: &mut CpuState, a: u32, b: u32, result: u32) {
    let cf = result < a;
    let of = (((a ^ result) & (b ^ result)) & 0x8000_0000) != 0;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    update_flags_arith32(state, result, cf, of, af);
}

fn update_flags_sub32(state: &mut CpuState, a: u32, b: u32, result: u32) {
    let cf = a < b;
    let of = (((a ^ b) & (a ^ result)) & 0x8000_0000) != 0;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    update_flags_arith32(state, result, cf, of, af);
}

fn update_flags_adc32(state: &mut CpuState, a: u32, b: u32, carry: u32, result: u32) {
    let sum = (a as u64) + (b as u64) + (carry as u64);
    let cf = sum > u32::MAX as u64;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    let sum_i = (a as i32 as i64) + (b as i32 as i64) + (carry as i64);
    let of = sum_i < i32::MIN as i64 || sum_i > i32::MAX as i64;
    update_flags_arith32(state, result, cf, of, af);
}

fn update_flags_sbb32(state: &mut CpuState, a: u32, b: u32, borrow: u32, result: u32) {
    let subtrahend = (b as u64) + (borrow as u64);
    let cf = (a as u64) < subtrahend;
    let af = ((a ^ b ^ result) & 0x10) != 0;
    let diff_i = (a as i32 as i64) - (b as i32 as i64) - (borrow as i64);
    let of = diff_i < i32::MIN as i64 || diff_i > i32::MAX as i64;
    update_flags_arith32(state, result, cf, of, af);
}

fn update_flags_logic32(state: &mut CpuState, result: u32) {
    let mut flags = state.rflags;
    flags &= !(FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF | FLAG_OF);
    flags |= FLAG_FIXED_1;

    if parity_even(result as u8) {
        flags |= FLAG_PF;
    }
    if result == 0 {
        flags |= FLAG_ZF;
    }
    if (result & 0x8000_0000) != 0 {
        flags |= FLAG_SF;
    }

    state.rflags = flags;
}

fn update_flags_arith(state: &mut CpuState, result: u64, cf: bool, of: bool, af: bool) {
    let mut flags = state.rflags;
    flags &= !(FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF);
    flags |= FLAG_FIXED_1;

    if cf {
        flags |= FLAG_CF;
    }
    if parity_even(result as u8) {
        flags |= FLAG_PF;
    }
    if af {
        flags |= FLAG_AF;
    }
    if result == 0 {
        flags |= FLAG_ZF;
    }
    if (result & 0x8000_0000_0000_0000) != 0 {
        flags |= FLAG_SF;
    }
    if of {
        flags |= FLAG_OF;
    }

    state.rflags = flags;
}

fn update_flags_arith32(state: &mut CpuState, result: u32, cf: bool, of: bool, af: bool) {
    let mut flags = state.rflags;
    flags &= !(FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF);
    flags |= FLAG_FIXED_1;

    if cf {
        flags |= FLAG_CF;
    }
    if parity_even(result as u8) {
        flags |= FLAG_PF;
    }
    if af {
        flags |= FLAG_AF;
    }
    if result == 0 {
        flags |= FLAG_ZF;
    }
    if (result & 0x8000_0000) != 0 {
        flags |= FLAG_SF;
    }
    if of {
        flags |= FLAG_OF;
    }

    state.rflags = flags;
}

fn update_flags_mul(state: &mut CpuState, overflow: bool) {
    let mut flags = state.rflags;
    flags &= !(FLAG_CF | FLAG_OF);
    flags |= FLAG_FIXED_1;
    if overflow {
        flags |= FLAG_CF | FLAG_OF;
    }
    state.rflags = flags;
}

fn update_flags_shift_u64(state: &mut CpuState, result: u64, cf: bool, of: Option<bool>) {
    let mut flags = state.rflags;
    flags &= !(FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF);
    if of.is_some() {
        flags &= !FLAG_OF;
    }
    flags |= FLAG_FIXED_1;

    if cf {
        flags |= FLAG_CF;
    }
    if parity_even(result as u8) {
        flags |= FLAG_PF;
    }
    if result == 0 {
        flags |= FLAG_ZF;
    }
    if (result & 0x8000_0000_0000_0000) != 0 {
        flags |= FLAG_SF;
    }
    if let Some(of) = of {
        if of {
            flags |= FLAG_OF;
        }
    }
    state.rflags = flags;
}

fn update_flags_rotate(state: &mut CpuState, cf: bool, of: Option<bool>) {
    let mut flags = state.rflags;
    flags &= !FLAG_CF;
    if of.is_some() {
        flags &= !FLAG_OF;
    }
    flags |= FLAG_FIXED_1;

    if cf {
        flags |= FLAG_CF;
    }
    if let Some(of) = of {
        if of {
            flags |= FLAG_OF;
        }
    }
    state.rflags = flags;
}

fn set_zf(state: &mut CpuState, zf: bool) {
    let mut flags = state.rflags;
    flags &= !FLAG_ZF;
    flags |= FLAG_FIXED_1;
    if zf {
        flags |= FLAG_ZF;
    }
    state.rflags = flags;
}

fn exec_shl_u64(state: &mut CpuState, count: u32) {
    let count = count & 0x3f;
    if count == 0 {
        return;
    }
    let value = state.rax;
    let result = value.wrapping_shl(count);
    state.rax = result;

    let cf = ((value >> (64 - count)) & 1) != 0;
    let of = if count == 1 {
        let msb = (result & 0x8000_0000_0000_0000) != 0;
        Some(msb ^ cf)
    } else {
        None
    };
    update_flags_shift_u64(state, result, cf, of);
}

fn exec_shr_u64(state: &mut CpuState, count: u32) {
    let count = count & 0x3f;
    if count == 0 {
        return;
    }
    let value = state.rax;
    let result = value.wrapping_shr(count);
    state.rax = result;

    let cf = ((value >> (count - 1)) & 1) != 0;
    let of = if count == 1 {
        Some((value & 0x8000_0000_0000_0000) != 0)
    } else {
        None
    };
    update_flags_shift_u64(state, result, cf, of);
}

fn exec_sar_u64(state: &mut CpuState, count: u32) {
    let count = count & 0x3f;
    if count == 0 {
        return;
    }
    let value = state.rax;
    let result = ((value as i64) >> count) as u64;
    state.rax = result;

    let cf = ((value >> (count - 1)) & 1) != 0;
    let of = if count == 1 { Some(false) } else { None };
    update_flags_shift_u64(state, result, cf, of);
}

fn exec_rol_u64(state: &mut CpuState, count: u32) {
    let count = count & 0x3f;
    if count == 0 {
        return;
    }
    let result = state.rax.rotate_left(count);
    state.rax = result;

    let cf = (result & 1) != 0;
    let of = if count == 1 {
        let msb = (result & 0x8000_0000_0000_0000) != 0;
        Some(msb ^ cf)
    } else {
        None
    };
    update_flags_rotate(state, cf, of);
}

fn exec_ror_u64(state: &mut CpuState, count: u32) {
    let count = count & 0x3f;
    if count == 0 {
        return;
    }
    let result = state.rax.rotate_right(count);
    state.rax = result;

    let cf = (result & 0x8000_0000_0000_0000) != 0;
    let of = if count == 1 {
        let msb = (result >> 63) & 1;
        let msb2 = (result >> 62) & 1;
        Some((msb ^ msb2) != 0)
    } else {
        None
    };
    update_flags_rotate(state, cf, of);
}

fn exec_bsf_u64(state: &mut CpuState) {
    let src = state.rbx;
    if src == 0 {
        set_zf(state, true);
        return;
    }
    state.rax = src.trailing_zeros() as u64;
    set_zf(state, false);
}

fn exec_bsr_u64(state: &mut CpuState) {
    let src = state.rbx;
    if src == 0 {
        set_zf(state, true);
        return;
    }
    state.rax = (63 - src.leading_zeros()) as u64;
    set_zf(state, false);
}

fn parity_even(byte: u8) -> bool {
    (byte.count_ones() & 1) == 0
}
