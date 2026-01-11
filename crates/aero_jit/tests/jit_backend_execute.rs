#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::jit::runtime::{CompileRequestSink, JitBackend, JitConfig, JitRuntime};
use aero_cpu_core::state::CpuState;
use aero_jit::backend::WasmtimeBackend;
use aero_jit::tier1::ir::{GuestReg, IrBuilder, IrTerminator};
use aero_jit::tier1::wasm::Tier1WasmCodegen;
use aero_jit::tier1::{discover_block, translate_block, BlockLimits};
use aero_jit::Tier1Bus;
use aero_types::{Gpr, Width};

#[derive(Default)]
struct NullCompileSink;

impl CompileRequestSink for NullCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {}
}

#[test]
fn jit_backend_executes_tier1_block_by_table_index() {
    // mov eax, 5
    // add eax, 7
    // ret
    let code = [
        0xb8, 0x05, 0x00, 0x00, 0x00, // mov eax, 5
        0x83, 0xc0, 0x07, // add eax, 7
        0xc3, // ret
    ];

    let entry = 0x1000u64;

    let mut backend = WasmtimeBackend::<CpuState>::new();
    for (i, b) in code.iter().enumerate() {
        backend.write_u8(entry + i as u64, *b);
    }
    // Return address for RET.
    backend.write(0x8000, Width::W64, 0x2000);

    let block = discover_block(&backend, entry, BlockLimits::default());
    let ir = translate_block(&block);
    let wasm = Tier1WasmCodegen::new().compile_block(&ir);

    let table_index = backend.add_compiled_block(&wasm);

    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let mut jit = JitRuntime::new(config, backend, NullCompileSink::default());
    jit.install_block(entry, table_index, entry, code.len() as u32);

    let mut cpu = CpuState::default();
    cpu.rip = entry;
    cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x8000;

    let handle = jit.prepare_block(entry).expect("expected compiled handle");
    let exit = jit.execute_block(&mut cpu, &handle);

    assert_eq!(exit.next_rip, 0x2000);
    assert!(!exit.exit_to_interpreter);
    assert_eq!(cpu.rip, 0x2000);
    assert_eq!(cpu.gpr[Gpr::Rax.as_u8() as usize], 12);
    assert_eq!(cpu.gpr[Gpr::Rsp.as_u8() as usize], 0x8008);
}

#[test]
fn jit_backend_exit_to_interpreter_uses_sentinel() {
    let entry = 0x4000u64;
    let next_rip = 0x5000u64;

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
    let ir = b.finish(IrTerminator::ExitToInterpreter { next_rip });
    let wasm = Tier1WasmCodegen::new().compile_block(&ir);

    let mut backend = WasmtimeBackend::<CpuState>::new();
    let table_index = backend.add_compiled_block(&wasm);

    let mut cpu = CpuState::default();
    cpu.rip = entry;

    let exit = backend.execute(table_index, &mut cpu);

    assert_eq!(exit.next_rip, next_rip);
    assert!(exit.exit_to_interpreter);
    assert_eq!(cpu.rip, next_rip);
    assert_eq!(cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1234);
}
