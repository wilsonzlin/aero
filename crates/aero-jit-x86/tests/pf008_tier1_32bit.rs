#![cfg(not(target_arch = "wasm32"))]

use std::collections::{HashSet, VecDeque};

use aero_cpu_core::jit::runtime::{JitConfig, JitRuntime};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_jit_x86::backend::{Tier1Cpu, WasmBackend};
use aero_jit_x86::tier1_pipeline::{Tier1CompileQueue, Tier1Compiler};
use aero_jit_x86::{discover_block_mode, BlockLimits, Tier1Bus};
use aero_types::{Gpr, Width};
use aero_x86::tier1::InstKind;

#[derive(Debug, Default)]
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

fn collect_block_entries<B: Tier1Bus>(bus: &B, entry_rip: u64, bitness: u32) -> Vec<u64> {
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();

    queue.push_back(entry_rip);
    seen.insert(entry_rip);

    while let Some(rip) = queue.pop_front() {
        let block = discover_block_mode(bus, rip, BlockLimits::default(), bitness);
        let Some(last) = block.insts.last() else {
            panic!("decoded empty block at 0x{rip:x}");
        };

        match &last.kind {
            InstKind::JmpRel { target } => {
                if seen.insert(*target) {
                    queue.push_back(*target);
                }
            }
            InstKind::JccRel { target, .. } => {
                if seen.insert(*target) {
                    queue.push_back(*target);
                }
                let fallthrough = last.next_rip();
                if seen.insert(fallthrough) {
                    queue.push_back(fallthrough);
                }
            }
            InstKind::CallRel { target } => {
                if seen.insert(*target) {
                    queue.push_back(*target);
                }
                let fallthrough = last.next_rip();
                if seen.insert(fallthrough) {
                    queue.push_back(fallthrough);
                }
            }
            InstKind::Ret => {}
            InstKind::Invalid => {
                panic!("Tier-1 decode produced Invalid at 0x{rip:x}");
            }
            other => {
                panic!("unexpected block terminator at 0x{rip:x}: {other:?}");
            }
        }
    }

    let mut out: Vec<u64> = seen.into_iter().collect();
    out.sort_unstable();
    out
}

fn run_pf008_payload_32(payload: &[u8], expected_checksum: u32, scratch: Option<(u64, usize)>) {
    let bitness = 32;
    let iters: u32 = 10_000;

    // Keep guest code + stack below 0x1_0000 so we don't overlap the backend's CpuState ABI region.
    let code_base = 0x1000u64;
    let stack_top = 0x8000u64;
    let sentinel_ret: u64 = 0x9000;

    let mut backend: WasmBackend<TestCpu> = WasmBackend::new();
    for (i, b) in payload.iter().enumerate() {
        backend.write_u8(code_base + i as u64, *b);
    }
    backend.write(stack_top, Width::W32, sentinel_ret);
    if let Some((base, len)) = scratch {
        for i in 0..len {
            backend.write_u8(base + i as u64, 0);
        }
    }

    let queue = Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 64,
        cache_max_bytes: 0,
    };
    let mut jit: JitRuntime<WasmBackend<TestCpu>, Tier1CompileQueue> =
        JitRuntime::new(config, backend.clone(), queue);

    // Compile all blocks reachable from the entry point.
    let mut compiler = Tier1Compiler::new(backend.clone(), backend.clone());
    for rip in collect_block_entries(&backend, code_base, bitness) {
        let handle = compiler
            .compile_handle(&jit, rip, bitness)
            .unwrap_or_else(|e| panic!("compile_handle failed for rip=0x{rip:x}: {e}"));
        jit.install_handle(handle);
    }

    let mut cpu = TestCpu {
        state: CpuState::new(CpuMode::Protected),
    };
    cpu.state.rip = code_base;
    cpu.state.gpr[Gpr::Rsp.as_u8() as usize] = stack_top;
    cpu.state.gpr[Gpr::Rcx.as_u8() as usize] = iters as u64;
    if let Some((base, _len)) = scratch {
        // PF-008 32-bit memory payloads use EDI as the scratch base register.
        cpu.state.gpr[Gpr::Rdi.as_u8() as usize] = base;
    }

    // Run until the payload returns to the sentinel.
    let mut steps = 0u64;
    while cpu.state.rip != sentinel_ret {
        steps += 1;
        assert!(steps < 1_000_000, "execution did not terminate");

        let entry = cpu.state.rip;
        let handle = jit
            .prepare_block(entry)
            .unwrap_or_else(|| panic!("missing compiled block for rip=0x{entry:x}"));
        let exit = jit.execute_block(&mut cpu, &handle);
        assert!(
            !exit.exit_to_interpreter,
            "unexpected exit-to-interpreter at rip=0x{entry:x}"
        );
        cpu.state.rip = exit.next_rip;
    }

    let eax = cpu.state.gpr[Gpr::Rax.as_u8() as usize] as u32;
    assert_eq!(eax, expected_checksum);

    // We should have popped exactly one 32-bit return address.
    assert_eq!(
        cpu.state.gpr[Gpr::Rsp.as_u8() as usize],
        stack_top + 4
    );
}

#[test]
fn pf008_alu32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `alu32` payload bytes.
    let payload: &[u8] = &[
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xba, 0x15, 0x7c, 0x4a, 0x7f, 0x01, 0xd0, 0x89, 0xc3,
        0xc1, 0xeb, 0x0d, 0x31, 0xd8, 0xd1, 0xe0, 0x49, 0x75, 0xf2, 0xc3,
    ];
    run_pf008_payload_32(payload, 0x30aae0b8, None);
}

#[test]
fn pf008_call_ret32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `call_ret32` payload bytes.
    let payload: &[u8] = &[
        0xb8, 0xef, 0xbe, 0xfe, 0xca, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0xe8, 0x04, 0x00, 0x00,
        0x00, 0x49, 0x75, 0xf8, 0xc3, 0x53, 0x56, 0x01, 0xd8, 0x35, 0xb5, 0x3b, 0x12, 0x1f,
        0xc1, 0xe0, 0x03, 0x5e, 0x5b, 0xc3,
    ];
    run_pf008_payload_32(payload, 0x71df5500, None);
}

#[test]
fn pf008_branch_pred32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `branch_pred32` payload bytes.
    let payload: &[u8] = &[
        0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0x31, 0xd2, 0x75, 0x02,
        0x01, 0xd8, 0x31, 0xd2, 0x75, 0x02, 0x31, 0xd8, 0xd1, 0xe0, 0x83, 0xc0, 0x01, 0x49,
        0x75, 0xec, 0xc3,
    ];
    run_pf008_payload_32(payload, 0xaad6_afab, None);
}

#[test]
fn pf008_mem_seq32_checksum() {
    // From `docs/16-guest-cpu-benchmark-suite.md` (PF-008), `mem_seq32` payload bytes.
    let payload: &[u8] = &[
        0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2,
        0x89, 0x14, 0x37, 0x83, 0xc6, 0x04, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75,
        0xea, 0xc3,
    ];
    let scratch_base = 0xa000u64;
    run_pf008_payload_32(payload, 0x0cc5_0aff, Some((scratch_base, 4096)));
}
