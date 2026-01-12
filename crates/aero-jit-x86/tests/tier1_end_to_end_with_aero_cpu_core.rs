#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome,
};
use aero_cpu_core::jit::runtime::{JitConfig, JitRuntime};
use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::backend::{Tier1Cpu, WasmBackend};
use aero_jit_x86::tier1::ir::interp as tier1_interp;
use aero_jit_x86::tier1::pipeline::{Tier1CompileQueue, Tier1Compiler};
use aero_jit_x86::tier1::Tier1WasmOptions;
use aero_jit_x86::{discover_block, translate_block, BlockLimits, Tier1Bus};
use aero_types::Gpr;

#[derive(Default)]
struct TestCpu {
    state: CpuState,
}

impl Tier1Cpu for TestCpu {
    fn tier1_state(&self) -> &CpuState {
        &self.state
    }

    fn tier1_state_mut(&mut self) -> &mut CpuState {
        &mut self.state
    }
}

impl ExecCpu for TestCpu {
    fn rip(&self) -> u64 {
        self.state.rip
    }

    fn set_rip(&mut self, rip: u64) {
        self.state.rip = rip;
    }

    fn maybe_deliver_interrupt(&mut self) -> bool {
        false
    }
}

struct Tier1Interpreter {
    bus: WasmBackend<TestCpu>,
}

impl Interpreter<TestCpu> for Tier1Interpreter {
    fn exec_block(&mut self, cpu: &mut TestCpu) -> InterpreterBlockExit {
        let entry_rip = cpu.rip();
        let block = discover_block(&self.bus, entry_rip, BlockLimits::default());
        let mut instructions_retired = block.insts.len() as u64;
        if matches!(block.end_kind, aero_jit_x86::BlockEndKind::ExitToInterpreter { .. }) {
            // Tier-1 discovery includes the invalid instruction that triggers the bailout, but the
            // translator emits `ExitToInterpreter` without executing/retiring it.
            instructions_retired = instructions_retired.saturating_sub(1);
        }
        let ir = translate_block(&block);
        let mut cpu_mem = vec![0u8; abi::CPU_STATE_SIZE as usize];
        tier1_interp::TestCpu::from_cpu_state(&cpu.state).write_to_abi_mem(&mut cpu_mem);
        let _ = tier1_interp::execute_block(&ir, &mut cpu_mem, &mut self.bus);
        tier1_interp::TestCpu::from_abi_mem(&cpu_mem).write_to_cpu_state(&mut cpu.state);
        InterpreterBlockExit {
            next_rip: cpu.state.rip,
            instructions_retired,
        }
    }
}

#[test]
fn tier1_hotness_triggers_compile_and_subsequent_execution_uses_jit() {
    // A tight loop:
    //   mov eax, 0
    //   add eax, 1
    //   cmp eax, 0
    //   jne <entry>
    //
    // Encoded as:
    //   b8 00 00 00 00
    //   83 c0 01
    //   83 f8 00
    //   75 f3   ; -13 bytes
    let code = [
        0xb8, 0x00, 0x00, 0x00, 0x00, // mov eax, 0
        0x83, 0xc0, 0x01, // add eax, 1
        0x83, 0xf8, 0x00, // cmp eax, 0
        0x75, 0xf3, // jne -13
    ];
    let entry = 0x1000u64;

    let mut backend: WasmBackend<TestCpu> = WasmBackend::new();
    for (i, b) in code.iter().enumerate() {
        backend.write_u8(entry + i as u64, *b);
    }

    let interpreter = Tier1Interpreter {
        bus: backend.clone(),
    };

    let queue = Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        hot_threshold: 3,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend.clone(), queue.clone());
    let mut dispatcher = ExecDispatcher::new(interpreter, jit);

    let mut cpu = TestCpu::default();
    cpu.state.rip = entry;

    // Run until the runtime requests compilation.
    for _ in 0..10 {
        let outcome = dispatcher.step(&mut cpu);
        assert!(matches!(
            outcome,
            StepOutcome::Block {
                tier: ExecutedTier::Interpreter,
                ..
            }
        ));

        if !queue.is_empty() {
            break;
        }
    }

    let requested = queue.drain();
    assert_eq!(requested, vec![entry]);

    let mut compiler = Tier1Compiler::new(backend.clone(), backend.clone())
        .with_wasm_options(Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        });
    for rip in requested {
        compiler
            .compile_and_install(dispatcher.jit_mut(), rip, 64)
            .unwrap();
    }

    // Prove we actually executed the compiled block by seeding RAX to a different value and
    // checking the block's `mov eax, 0; add eax, 1` sequence ran.
    cpu.state.gpr[Gpr::Rax.as_u8() as usize] = 0x1234;

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            entry_rip,
            next_rip,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(entry_rip, entry);
            assert_eq!(next_rip, entry);
        }
        other => panic!("expected block execution, got {other:?}"),
    }

    assert_eq!(cpu.state.gpr[Gpr::Rax.as_u8() as usize], 1);
    assert!(queue.is_empty());
}
