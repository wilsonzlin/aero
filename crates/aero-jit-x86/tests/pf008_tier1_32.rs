#![cfg(not(target_arch = "wasm32"))]

//! PF-008 Tier-1 JIT correctness regression tests (32-bit payloads).
//!
//! These tests execute the canonical PF-008 32-bit payload byte streams (from
//! `docs/16-guest-cpu-benchmark-suite.md`) through the *real* tiered runtime
//! path (`ExecDispatcher` + `JitRuntime` + `WasmBackend`) and assert the final
//! checksum in `EAX` (stored in `CpuState.gpr[RAX]`).

use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome,
};
use aero_cpu_core::jit::runtime::{JitConfig, JitRuntime};
use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::backend::{Tier1Cpu, WasmBackend};
use aero_jit_x86::tier1::ir::interp as tier1_interp;
use aero_jit_x86::tier1::pipeline::{Tier1CompileQueue, Tier1Compiler};
use aero_jit_x86::{discover_block_mode, translate_block, BlockLimits, Tier1Bus};
use aero_types::{Gpr, Width};

const ITERS_PER_RUN: u64 = 10_000;

const ENTRY_RIP: u64 = 0x1000;
const SCRATCH_BASE: u64 = 0x8000;
const SCRATCH_LEN: usize = 4096;

const STACK_RSP: u64 = 0xf000;
// Sentinel return address used to stop execution when the payload returns via `ret`.
// Must not be `u64::MAX` since Tier-1 uses `u64::MAX` as an internal JIT exit sentinel.
const RETURN_SENTINEL_RIP: u64 = 0xdead_beef;

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
        let block = discover_block_mode(&self.bus, entry_rip, BlockLimits::default(), 32);
        let mut instructions_retired = block.insts.len() as u64;
        if matches!(block.end_kind, aero_jit_x86::BlockEndKind::ExitToInterpreter { .. }) {
            // Tier-1 discovery includes the invalid instruction that triggers the bailout, but the
            // translator emits `ExitToInterpreter` without executing/retiring it.
            instructions_retired = instructions_retired.saturating_sub(1);
        }
        let ir = translate_block(&block);

        let mut cpu_mem = vec![0u8; abi::CPU_STATE_SIZE as usize];
        tier1_interp::TestCpu::from_cpu_state(&cpu.state).write_to_abi_mem(&mut cpu_mem);

        match tier1_interp::execute_block(&ir, &mut cpu_mem, &mut self.bus) {
            tier1_interp::ExecResult::Continue => {}
            tier1_interp::ExecResult::ExitToInterpreter { next_rip } => {
                panic!(
                    "unexpected Tier-1 interpreter bailout at 0x{next_rip:x}\nIR:\n{}",
                    ir.to_text()
                );
            }
        }

        tier1_interp::TestCpu::from_abi_mem(&cpu_mem).write_to_cpu_state(&mut cpu.state);
        InterpreterBlockExit {
            next_rip: cpu.state.rip,
            instructions_retired,
        }
    }
}

fn load_bytes<B: Tier1Bus>(bus: &mut B, addr: u64, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        bus.write_u8(addr + i as u64, *b);
    }
}

fn zero_mem<B: Tier1Bus>(bus: &mut B, addr: u64, len: usize) {
    for i in 0..len {
        bus.write_u8(addr + i as u64, 0);
    }
}

fn run_pf008_payload_32(
    variant: &'static str,
    code: &[u8],
    expected_checksum: u64,
    needs_scratch: bool,
) {
    // --- Guest memory ---
    let mut backend: WasmBackend<TestCpu> = WasmBackend::new();
    load_bytes(&mut backend, ENTRY_RIP, code);
    backend.write(STACK_RSP, Width::W32, RETURN_SENTINEL_RIP);
    if needs_scratch {
        zero_mem(&mut backend, SCRATCH_BASE, SCRATCH_LEN);
    }

    // --- Tiered runtime wiring ---
    let interpreter = Tier1Interpreter {
        bus: backend.clone(),
    };
    let queue = Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        // Compile blocks eagerly so the bulk of the payload runs in Tier-1 JIT.
        hot_threshold: 1,
        cache_max_blocks: 4096,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend.clone(), queue.clone());
    let mut dispatcher = ExecDispatcher::new(interpreter, jit);

    let mut compiler =
        Tier1Compiler::new(backend.clone(), backend.clone()).with_limits(BlockLimits::default());

    // --- CPU state (PF-008 ABI) ---
    let mut cpu = TestCpu::default();
    cpu.state.rip = ENTRY_RIP;
    cpu.state.gpr[Gpr::Rsp.as_u8() as usize] = STACK_RSP;
    cpu.state.gpr[Gpr::Rcx.as_u8() as usize] = ITERS_PER_RUN;
    if needs_scratch {
        cpu.state.gpr[Gpr::Rdi.as_u8() as usize] = SCRATCH_BASE;
    }

    // --- Execute until `ret` returns to sentinel RIP ---
    let mut saw_jit = false;
    let mut steps = 0u64;
    let max_steps = ITERS_PER_RUN
        .saturating_mul(10)
        .saturating_add(10_000);

    while cpu.state.rip != RETURN_SENTINEL_RIP {
        steps += 1;
        assert!(
            steps <= max_steps,
            "{variant}: did not reach sentinel ret RIP (rip=0x{:x})",
            cpu.state.rip
        );

        match dispatcher.step(&mut cpu) {
            StepOutcome::InterruptDelivered => {}
            StepOutcome::Block { tier, .. } => {
                if tier == ExecutedTier::Jit {
                    saw_jit = true;
                }
            }
        }

        for rip in queue.drain() {
            compiler
                .compile_and_install(dispatcher.jit_mut(), rip, 32)
                .unwrap();
        }
    }

    assert!(
        saw_jit,
        "{variant}: expected at least one block to execute in Tier-1 JIT"
    );

    assert_eq!(
        cpu.state.gpr[Gpr::Rax.as_u8() as usize],
        expected_checksum,
        "{variant}: checksum mismatch"
    );
}

#[test]
fn pf008_alu32_checksum_tier1_jit() {
    let code = [
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xba, 0x15, 0x7c, 0x4a, 0x7f, 0x01, 0xd0, 0x89, 0xc3,
        0xc1, 0xeb, 0x0d, 0x31, 0xd8, 0xd1, 0xe0, 0x49, 0x75, 0xf2, 0xc3,
    ];
    run_pf008_payload_32("alu32", &code, 0x30aae0b8, false);
}

#[test]
fn pf008_branch_pred32_checksum_tier1_jit() {
    let code = [
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0x31, 0xd2, 0x75, 0x02,
        0x01, 0xd8, 0x31, 0xd2, 0x75, 0x02, 0x31, 0xd8, 0xd1, 0xe0, 0x83, 0xc0, 0x01, 0x49,
        0x75, 0xec, 0xc3,
    ];
    run_pf008_payload_32("branch_pred32", &code, 0xaad6afab, false);
}

#[test]
fn pf008_branch_unpred32_checksum_tier1_jit() {
    let code = [
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x08, 0x09, 0x0a, 0x0b, 0x89, 0xc2, 0xc1, 0xe2,
        0x0d, 0x31, 0xd0, 0x89, 0xc2, 0xc1, 0xea, 0x07, 0x31, 0xd0, 0x89, 0xc2, 0xc1, 0xe2,
        0x11, 0x31, 0xd0, 0x89, 0xc2, 0x83, 0xe2, 0x01, 0x74, 0x04, 0x01, 0xc3, 0xeb, 0x04,
        0x31, 0xc3, 0xeb, 0x00, 0x49, 0x75, 0xd9, 0x89, 0xd8, 0xc3,
    ];
    run_pf008_payload_32("branch_unpred32", &code, 0xb1fdf341, false);
}

#[test]
fn pf008_mem_seq32_checksum_tier1_jit() {
    let code = [
        0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2,
        0x89, 0x14, 0x37, 0x83, 0xc6, 0x04, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75,
        0xea, 0xc3,
    ];
    run_pf008_payload_32("mem_seq32", &code, 0x0cc50aff, true);
}

#[test]
fn pf008_mem_stride32_checksum_tier1_jit() {
    let code = [
        0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2,
        0x89, 0x14, 0x37, 0x83, 0xc6, 0x40, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75,
        0xea, 0xc3,
    ];
    run_pf008_payload_32("mem_stride32", &code, 0x0da7ebb4, true);
}

#[test]
fn pf008_call_ret32_checksum_tier1_jit() {
    let code = [
        0xb8, 0xef, 0xbe, 0xfe, 0xca, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0xe8, 0x04, 0x00, 0x00,
        0x00, 0x49, 0x75, 0xf8, 0xc3, 0x53, 0x56, 0x01, 0xd8, 0x35, 0xb5, 0x3b, 0x12, 0x1f,
        0xc1, 0xe0, 0x03, 0x5e, 0x5b, 0xc3,
    ];
    run_pf008_payload_32("call_ret32", &code, 0x71df5500, false);
}
