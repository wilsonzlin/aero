#![no_main]

use aero_cpu_core::interp::tier0::{exec::step_with_config, Tier0Config};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{gpr, CpuMode, CpuState, RFLAGS_RESERVED1};
use libfuzzer_sys::fuzz_target;

const RAM_SIZE: usize = 64 * 1024;
const CODE_ADDR: u64 = 0x1000;

/// Read an `u64` from `data[offset..offset+8]` as little-endian, zero-padding short inputs.
fn parse_u64_le(data: &[u8], offset: usize) -> u64 {
    let mut buf = [0u8; 8];
    let Some(src) = data.get(offset..) else {
        return 0;
    };
    let n = src.len().min(8);
    buf[..n].copy_from_slice(&src[..n]);
    u64::from_le_bytes(buf)
}

fn cap_addr(v: u64) -> u64 {
    // Keep memory operands inside our fixed FlatTestBus address space so we spend
    // more time in the interpreter rather than immediately faulting.
    v % (RAM_SIZE as u64)
}

fuzz_target!(|data: &[u8]| {
    // Layout (bounded):
    //   0x000..0x100: CPU state seed (mode, regs, flags, control regs)
    //   0x100..end:   guest RAM image (up to 64KiB; extra input ignored)
    const SEED_SIZE: usize = 0x100;

    let seed_len = data.len().min(SEED_SIZE);
    let seed = &data[..seed_len];
    let ram_blob = &data[seed_len..];

    let mode = match seed.get(0).copied().unwrap_or(0) % 3 {
        0 => CpuMode::Real,
        1 => CpuMode::Protected,
        _ => CpuMode::Long,
    };

    let mut cpu = CpuState::new(mode);
    cpu.halted = false;
    cpu.pending_bios_int_valid = false;

    // Flags + GPRs.
    cpu.rflags = parse_u64_le(seed, 0x01) | RFLAGS_RESERVED1;
    for i in 0..cpu.gpr.len() {
        cpu.gpr[i] = parse_u64_le(seed, 0x09 + i * 8);
    }

    // Keep repeat counts bounded to avoid very slow single-step REP string ops.
    let rep_count = (parse_u64_le(seed, 0x89) % 128) as u64;
    cpu.gpr[gpr::RCX] = rep_count;

    // Keep common address registers inside RAM so we can hit more RMW/mem paths.
    cpu.gpr[gpr::RSP] = cap_addr(cpu.gpr[gpr::RSP]);
    cpu.gpr[gpr::RBP] = cap_addr(cpu.gpr[gpr::RBP]);
    cpu.gpr[gpr::RSI] = cap_addr(cpu.gpr[gpr::RSI]);
    cpu.gpr[gpr::RDI] = cap_addr(cpu.gpr[gpr::RDI]);

    // Control regs/MSRs: keep mostly input-driven but bounded where possible.
    cpu.control.cr0 = parse_u64_le(seed, 0x90);
    cpu.control.cr3 = cap_addr(parse_u64_le(seed, 0x98)) & !0xfff;
    cpu.control.cr4 = parse_u64_le(seed, 0xA0);
    cpu.msr.efer = parse_u64_le(seed, 0xA8);

    // Ensure we fetch from a fixed RIP inside our RAM.
    cpu.segments.cs.base = 0;
    cpu.segments.cs.selector = 0;
    cpu.set_rip(CODE_ADDR);

    let mut bus = FlatTestBus::new(RAM_SIZE);

    // Initialize RAM contents from the input blob (bounded to RAM_SIZE).
    let ram_init_len = ram_blob.len().min(RAM_SIZE);
    if ram_init_len != 0 {
        bus.load(0, &ram_blob[..ram_init_len]);
    }

    // Always install an instruction window at a known address so decode/exec runs
    // even on short inputs.
    let code_src = if !ram_blob.is_empty() { ram_blob } else { data };
    let mut instr = [0u8; 15];
    let n = code_src.len().min(instr.len());
    instr[..n].copy_from_slice(&code_src[..n]);
    bus.load(CODE_ADDR, &instr);

    let cfg = Tier0Config::default();
    let _ = step_with_config(&cfg, &mut cpu, &mut bus);
});

