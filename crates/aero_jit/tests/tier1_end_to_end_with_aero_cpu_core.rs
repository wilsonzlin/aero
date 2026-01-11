#![cfg(not(target_arch = "wasm32"))]

use aero_cpu::{CpuBus, CpuState};
use aero_cpu_core::exec::{ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, StepOutcome};
use aero_cpu_core::jit::runtime::{JitConfig, JitRuntime};
use aero_jit::backend::{compile_and_install, CompileQueue, Tier1Cpu, WasmBackend};
use aero_jit::tier1_ir::interp::execute_block;
use aero_jit::{discover_block, translate_block, BlockLimits};
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
    fn exec_block(&mut self, cpu: &mut TestCpu) -> u64 {
        let entry_rip = cpu.rip();
        let block = discover_block(&self.bus, entry_rip, BlockLimits::default());
        let ir = translate_block(&block);
        let _ = execute_block(&ir, &mut cpu.state, &mut self.bus);
        cpu.state.rip
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

    let interpreter = Tier1Interpreter { bus: backend.clone() };

    let queue = CompileQueue::default();
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

    assert_eq!(queue.snapshot(), vec![entry]);
    let requested = queue.drain();

    for rip in requested {
        let handle = {
            let jit = dispatcher.jit_mut();
            compile_and_install(&mut backend, jit, rip)
        };
        dispatcher.jit_mut().install_handle(handle);
    }

    // Prove we actually executed the compiled block by seeding RAX to a different value and
    // checking the block's `mov eax, 0; add eax, 1` sequence ran.
    cpu.state.write_gpr(Gpr::Rax, 0x1234);

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            entry_rip,
            next_rip,
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(entry_rip, entry);
            assert_eq!(next_rip, entry);
        }
        other => panic!("expected block execution, got {other:?}"),
    }

    assert_eq!(cpu.state.read_gpr(Gpr::Rax), 1);
    assert!(queue.is_empty());
}
