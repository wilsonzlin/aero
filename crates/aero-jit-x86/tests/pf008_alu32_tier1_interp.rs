#![cfg(debug_assertions)]

mod tier1_common;

use std::collections::HashMap;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::interp::{execute_block, ExecResult};
use aero_jit_x86::{discover_block_mode, translate_block, BlockLimits};
use aero_types::Gpr;
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot, SimpleBus};

fn run_payload_32(
    entry: u64,
    stop_rip: u64,
    code: &[u8],
    iters: u32,
    mut cpu: CpuState,
    mut bus: SimpleBus,
) -> CpuSnapshot {
    cpu.rip = entry;
    cpu.gpr[Gpr::Rcx.as_u8() as usize] = iters as u64;

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);

    bus.load(entry, code);

    let mut cache = HashMap::new();
    let mut steps = 0usize;

    loop {
        let snap = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
        if snap.rip == stop_rip {
            return snap;
        }

        steps += 1;
        // Conservative upper bound: these payloads are small loops with at most a handful of
        // blocks per iteration.
        assert!(
            steps <= (iters as usize).saturating_mul(8).saturating_add(10_000),
            "loop did not reach stop_rip (rip=0x{:x})",
            snap.rip
        );

        // If we're approaching `stop_rip`, cap the block length so we don't accidentally execute
        // the `ret` instruction when the last basic block contains non-terminator instructions
        // before it (e.g. `mov eax, ebx; ret` in `branch_unpred32`).
        let mut limits = BlockLimits::default();
        if snap.rip < stop_rip {
            let max_bytes_to_stop = (stop_rip - snap.rip) as usize;
            limits.max_bytes = limits.max_bytes.min(max_bytes_to_stop);
        }

        let ir = cache.entry(snap.rip).or_insert_with(|| {
            let block = discover_block_mode(&bus, snap.rip, limits, 32);
            translate_block(&block)
        });

        match execute_block(ir, &mut cpu_bytes, &mut bus) {
            ExecResult::Continue => {}
            ExecResult::ExitToInterpreter { next_rip } => {
                if next_rip == stop_rip {
                    return CpuSnapshot::from_wasm_bytes(&cpu_bytes);
                }
                panic!(
                    "unexpected Tier-1 bailout at 0x{next_rip:x}\nIR:\n{}",
                    ir.to_text()
                );
            }
        }
    }
}

#[test]
fn pf008_alu32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `alu32` payload.
    //
    // BITS 32
    // mov eax, 0x9ABCDEF0
    // mov edx, 0x7F4A7C15
    // .loop:
    //   add eax, edx
    //   mov ebx, eax
    //   shr ebx, 13
    //   xor eax, ebx
    //   shl eax, 1
    //   dec ecx
    //   jnz .loop
    // ret
    let code = [
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xba, 0x15, 0x7c, 0x4a, 0x7f, 0x01, 0xd0, 0x89, 0xc3,
        0xc1, 0xeb, 0x0d, 0x31, 0xd8, 0xd1, 0xe0, 0x49, 0x75, 0xf2, 0xc3,
    ];

    let entry = 0x1000u64;
    let ret_rip = entry + (code.len() as u64 - 1);

    let cpu = CpuState::default();
    let bus = SimpleBus::new(0x20000);
    let final_cpu = run_payload_32(entry, ret_rip, &code, 10_000, cpu, bus);
    assert_eq!(final_cpu.gpr[Gpr::Rax.as_u8() as usize] as u32, 0x30aae0b8);
}

#[test]
fn pf008_mem_seq32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `mem_seq32` payload.
    let code = [
        0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2,
        0x89, 0x14, 0x37, 0x83, 0xc6, 0x04, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75,
        0xea, 0xc3,
    ];

    let entry = 0x2000u64;
    let ret_rip = entry + (code.len() as u64 - 1);

    let scratch_base = 0x10_000u64;

    let mut cpu = CpuState::default();
    cpu.gpr[Gpr::Rdi.as_u8() as usize] = scratch_base;

    let bus = SimpleBus::new(0x20_000);
    let final_cpu = run_payload_32(entry, ret_rip, &code, 10_000, cpu, bus);
    assert_eq!(final_cpu.gpr[Gpr::Rax.as_u8() as usize] as u32, 0x0cc50aff);
}

#[test]
fn pf008_call_ret32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `call_ret32` payload.
    let code = [
        0xb8, 0xef, 0xbe, 0xfe, 0xca, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0xe8, 0x04, 0x00, 0x00,
        0x00, 0x49, 0x75, 0xf8, 0xc3, 0x53, 0x56, 0x01, 0xd8, 0x35, 0xb5, 0x3b, 0x12, 0x1f,
        0xc1, 0xe0, 0x03, 0x5e, 0x5b, 0xc3,
    ];

    let entry = 0x3000u64;
    // The payload's "exit" ret is the first ret (after the loop), not the callee ret at the end.
    let stop_rip = entry + 18;

    let mut cpu = CpuState::default();
    cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x9000;

    let bus = SimpleBus::new(0x20_000);
    let final_cpu = run_payload_32(entry, stop_rip, &code, 10_000, cpu, bus);
    assert_eq!(final_cpu.gpr[Gpr::Rax.as_u8() as usize] as u32, 0x71df5500);
}

#[test]
fn pf008_branch_pred32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `branch_pred32` payload.
    let code = [
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0x31, 0xd2, 0x75, 0x02,
        0x01, 0xd8, 0x31, 0xd2, 0x75, 0x02, 0x31, 0xd8, 0xd1, 0xe0, 0x83, 0xc0, 0x01, 0x49,
        0x75, 0xec, 0xc3,
    ];

    let entry = 0x4000u64;
    let ret_rip = entry + (code.len() as u64 - 1);

    let cpu = CpuState::default();
    let bus = SimpleBus::new(0x20000);
    let final_cpu = run_payload_32(entry, ret_rip, &code, 10_000, cpu, bus);
    assert_eq!(final_cpu.gpr[Gpr::Rax.as_u8() as usize] as u32, 0xaad6afab);
}

#[test]
fn pf008_branch_unpred32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `branch_unpred32` payload.
    let code = [
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x08, 0x09, 0x0a, 0x0b, 0x89, 0xc2, 0xc1, 0xe2,
        0x0d, 0x31, 0xd0, 0x89, 0xc2, 0xc1, 0xea, 0x07, 0x31, 0xd0, 0x89, 0xc2, 0xc1, 0xe2,
        0x11, 0x31, 0xd0, 0x89, 0xc2, 0x83, 0xe2, 0x01, 0x74, 0x04, 0x01, 0xc3, 0xeb, 0x04,
        0x31, 0xc3, 0xeb, 0x00, 0x49, 0x75, 0xd9, 0x89, 0xd8, 0xc3,
    ];

    let entry = 0x5000u64;
    let ret_rip = entry + (code.len() as u64 - 1);

    let cpu = CpuState::default();
    let bus = SimpleBus::new(0x20000);
    let final_cpu = run_payload_32(entry, ret_rip, &code, 10_000, cpu, bus);
    assert_eq!(final_cpu.gpr[Gpr::Rax.as_u8() as usize] as u32, 0xb1fdf341);
}

#[test]
fn pf008_mem_stride32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `mem_stride32` payload.
    let code = [
        0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2,
        0x89, 0x14, 0x37, 0x83, 0xc6, 0x40, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75,
        0xea, 0xc3,
    ];

    let entry = 0x6000u64;
    let ret_rip = entry + (code.len() as u64 - 1);
    let scratch_base = 0x10_000u64;

    let mut cpu = CpuState::default();
    cpu.gpr[Gpr::Rdi.as_u8() as usize] = scratch_base;

    let bus = SimpleBus::new(0x20_000);
    let final_cpu = run_payload_32(entry, ret_rip, &code, 10_000, cpu, bus);
    assert_eq!(final_cpu.gpr[Gpr::Rax.as_u8() as usize] as u32, 0x0da7ebb4);
}
