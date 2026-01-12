#![cfg(debug_assertions)]

mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::interp::{execute_block, ExecResult};
use aero_jit_x86::{discover_block_mode, translate_block, BlockLimits};
use aero_types::Gpr;
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot, SimpleBus};

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

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rcx.as_u8() as usize] = 10_000;

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);

    let mut bus = SimpleBus::new(0x20000);
    bus.load(entry, &code);

    let mut steps = 0usize;
    loop {
        let snap = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
        if snap.rip == ret_rip {
            break;
        }
        steps += 1;
        assert!(steps <= 100_000, "loop did not reach ret (rip=0x{:x})", snap.rip);

        let block = discover_block_mode(&bus, snap.rip, BlockLimits::default(), 32);
        let ir = translate_block(&block);
        match execute_block(&ir, &mut cpu_bytes, &mut bus) {
            ExecResult::Continue => {}
            ExecResult::ExitToInterpreter { next_rip } => {
                panic!(
                    "unexpected Tier-1 bailout at 0x{next_rip:x}\nIR:\n{}",
                    ir.to_text()
                );
            }
        }
    }

    let final_cpu = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(final_cpu.gpr[Gpr::Rax.as_u8() as usize] as u32, 0x30aae0b8);
}
