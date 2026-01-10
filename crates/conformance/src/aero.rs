use crate::corpus::{TemplateKind, TestCase};
use crate::{
    CpuState, ExecOutcome, Fault, FLAG_AF, FLAG_CF, FLAG_FIXED_1, FLAG_OF, FLAG_PF, FLAG_SF,
    FLAG_ZF,
};

pub struct AeroBackend;

impl AeroBackend {
    pub fn new() -> Self {
        Self
    }

    pub fn execute(&mut self, case: &TestCase) -> ExecOutcome {
        let mut state = case.init;
        let mut memory = case.memory.clone();

        match execute_one(
            case.template.kind,
            &mut state,
            &mut memory,
            case.init.rdi,
            case.template.bytes.len(),
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
        TemplateKind::MovM64Rax => {
            write_u64(memory, mem_base, state.rdi, state.rax)?;
        }
        TemplateKind::MovRaxM64 => {
            state.rax = read_u64(memory, mem_base, state.rdi)?;
        }
        TemplateKind::AddM64Rax => {
            let a = read_u64(memory, mem_base, state.rdi)?;
            let b = state.rax;
            let result = a.wrapping_add(b);
            write_u64(memory, mem_base, state.rdi, result)?;
            update_flags_add(state, a, b, result);
        }
        TemplateKind::SubM64Rax => {
            let a = read_u64(memory, mem_base, state.rdi)?;
            let b = state.rax;
            let result = a.wrapping_sub(b);
            write_u64(memory, mem_base, state.rdi, result)?;
            update_flags_sub(state, a, b, result);
        }
    }

    state.rip = state.rip.wrapping_add(instr_len as u64);
    state.rflags |= FLAG_FIXED_1;
    Ok(())
}

fn read_u64(memory: &[u8], base: u64, addr: u64) -> Result<u64, Fault> {
    let offset = addr.checked_sub(base).ok_or(Fault::MemoryOutOfBounds)? as usize;
    let end = offset.checked_add(8).ok_or(Fault::MemoryOutOfBounds)?;
    if end > memory.len() {
        return Err(Fault::MemoryOutOfBounds);
    }
    let bytes: [u8; 8] = memory[offset..end]
        .try_into()
        .expect("slice length checked");
    Ok(u64::from_le_bytes(bytes))
}

fn write_u64(memory: &mut [u8], base: u64, addr: u64, value: u64) -> Result<(), Fault> {
    let offset = addr.checked_sub(base).ok_or(Fault::MemoryOutOfBounds)? as usize;
    let end = offset.checked_add(8).ok_or(Fault::MemoryOutOfBounds)?;
    if end > memory.len() {
        return Err(Fault::MemoryOutOfBounds);
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

fn parity_even(byte: u8) -> bool {
    (byte.count_ones() & 1) == 0
}
