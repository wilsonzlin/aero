#![cfg(debug_assertions)]

mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::interp::{execute_block, ExecResult};
use aero_jit_x86::tier1::ir::{GuestReg, IrBuilder, IrTerminator};
use aero_types::{Gpr, Width};
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot, SimpleBus};

#[test]
fn tier1_ir_interp_call_helper_bails_out_to_interpreter() {
    let entry = 0x1000u64;

    let mut b = IrBuilder::new(entry);
    let v = b.const_int(Width::W64, 0x1234);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W64,
            high8: false,
        },
        v,
    );
    b.call_helper("test_helper", Vec::new(), None);
    let ir = b.finish(IrTerminator::ExitToInterpreter {
        next_rip: entry + 4,
    });

    let mut cpu = CpuState::default();
    cpu.rip = entry;
    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);

    let mut bus = SimpleBus::new(0x10000);
    let res = execute_block(&ir, &mut cpu_bytes, &mut bus);
    assert!(matches!(
        res,
        ExecResult::ExitToInterpreter { next_rip } if next_rip == entry
    ));

    let out_cpu = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out_cpu.rip, entry);
    assert_eq!(out_cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1234);
}
