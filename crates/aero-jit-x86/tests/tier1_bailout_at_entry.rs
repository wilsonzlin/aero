#![cfg(not(target_arch = "wasm32"))]

mod tier1_common;

use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome,
};
use aero_cpu_core::jit::runtime::{JitBackend, JitBlockExit, JitConfig};
use aero_jit_x86::tier1::pipeline::{
    Tier1CompileError, Tier1CompileQueue, Tier1Compiler, Tier1WasmRegistry,
};
use aero_jit_x86::{BlockLimits, Tier1Bus};

use tier1_common::SimpleBus;

#[derive(Debug, Default)]
struct TestCpu {
    rip: u64,
}

impl ExecCpu for TestCpu {
    fn rip(&self) -> u64 {
        self.rip
    }

    fn set_rip(&mut self, rip: u64) {
        self.rip = rip;
    }

    fn maybe_deliver_interrupt(&mut self) -> bool {
        false
    }
}

/// Backend that should never be executed in this test (we assert the block was not installed).
#[derive(Debug, Default)]
struct PanicBackend;

impl JitBackend for PanicBackend {
    type Cpu = TestCpu;

    fn execute(&mut self, _table_index: u32, _cpu: &mut TestCpu) -> JitBlockExit {
        panic!("unexpected JIT execution: zero-progress block should not be installed")
    }
}

/// WASM registry that should never be invoked for a bailout-at-entry compilation result.
#[derive(Debug, Default)]
struct PanicRegistry;

impl Tier1WasmRegistry for PanicRegistry {
    fn register_tier1_block(&mut self, _wasm: Vec<u8>, _exit_to_interpreter: bool) -> u32 {
        panic!("unexpected tier1 block registration: expected bailout-at-entry")
    }
}

/// Tiny interpreter that supports:
/// - 0x90: NOP (unsupported by the Tier-1 decoder)
/// - 0xEB imm8: JMP rel8
struct MiniInterpreter {
    bus: SimpleBus,
}

impl Interpreter<TestCpu> for MiniInterpreter {
    fn exec_block(&mut self, cpu: &mut TestCpu) -> InterpreterBlockExit {
        let mut rip = cpu.rip();
        let mut instructions_retired = 0u64;
        loop {
            match self.bus.read_u8(rip) {
                0x90 => {
                    // NOP
                    rip = rip.wrapping_add(1);
                    instructions_retired += 1;
                }
                0xeb => {
                    // JMP rel8
                    let rel = self.bus.read_u8(rip.wrapping_add(1)) as i8;
                    let next = rip.wrapping_add(2);
                    rip = next.wrapping_add(rel as i64 as u64);
                    instructions_retired += 1;
                    break;
                }
                other => panic!("unexpected opcode 0x{other:02x} at 0x{rip:x}"),
            }
        }
        cpu.set_rip(rip);
        InterpreterBlockExit {
            next_rip: rip,
            instructions_retired,
        }
    }
}

#[test]
fn tier1_zero_progress_block_is_not_installed() {
    let entry = 0x1000u64;

    // A tight loop:
    //   nop
    //   jmp <entry>
    //
    // The Tier-1 decoder doesn't support 0x90 (NOP), so Tier-1 compilation will
    // produce an `ExitToInterpreter { next_rip: entry }` terminator with no
    // side-effecting IR instructions. This should be treated as "non-compilable"
    // to avoid JIT thrash.
    let code = [0x90, 0xeb, 0xfd]; // NOP; JMP -3

    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry, &code);

    let interpreter = MiniInterpreter { bus: bus.clone() };
    let queue = Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        hot_threshold: 3,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = aero_cpu_core::jit::runtime::JitRuntime::new(config, PanicBackend, queue.clone());
    let mut dispatcher = ExecDispatcher::new(interpreter, jit);

    let mut cpu = TestCpu { rip: entry };

    // Run blocks until hotness triggers compilation. Keep running to ensure the
    // request is only queued once.
    for _ in 0..5 {
        match dispatcher.step(&mut cpu) {
            StepOutcome::Block {
                tier,
                entry_rip,
                next_rip,
                instructions_retired: _,
            } => {
                assert_eq!(tier, ExecutedTier::Interpreter);
                assert_eq!(entry_rip, entry);
                assert_eq!(next_rip, entry);
            }
            StepOutcome::InterruptDelivered => panic!("unexpected interrupt"),
        }
    }

    assert_eq!(queue.drain(), vec![entry]);

    // Compilation should return `BailoutAtEntry` and must not install the block.
    let mut compiler = Tier1Compiler::new(bus, PanicRegistry).with_limits(BlockLimits {
        max_insts: 16,
        max_bytes: 64,
    });

    let res = compiler.compile_and_install(dispatcher.jit_mut(), entry);
    assert!(
        matches!(res, Err(Tier1CompileError::BailoutAtEntry { entry_rip }) if entry_rip == entry),
        "expected BailoutAtEntry, got {res:?}"
    );

    assert!(!dispatcher.jit_mut().is_compiled(entry));

    // Keep executing; we should stay on the interpreter path and not keep requesting compilation.
    for _ in 0..3 {
        match dispatcher.step(&mut cpu) {
            StepOutcome::Block { tier, .. } => assert_eq!(tier, ExecutedTier::Interpreter),
            StepOutcome::InterruptDelivered => panic!("unexpected interrupt"),
        }
    }
    assert!(queue.is_empty());
}
