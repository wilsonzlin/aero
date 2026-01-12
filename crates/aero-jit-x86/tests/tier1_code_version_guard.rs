#![cfg(not(target_arch = "wasm32"))]

mod tier1_common;

use aero_cpu_core::jit::runtime::{JitBackend, JitBlockExit, JitConfig, JitRuntime};
use aero_jit_x86::tier1_pipeline::{Tier1CompileQueue, Tier1Compiler, Tier1WasmRegistry};
use aero_jit_x86::BlockLimits;
use tier1_common::SimpleBus;

#[derive(Debug, Default)]
struct DummyCpu;

#[derive(Debug, Default)]
struct DummyBackend;

impl JitBackend for DummyBackend {
    type Cpu = DummyCpu;

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        unreachable!("backend execution is not used by this test")
    }
}

#[derive(Debug, Default)]
struct NoopRegistry;

impl Tier1WasmRegistry for NoopRegistry {
    fn register_tier1_block(&mut self, _wasm: Vec<u8>, _exit_to_interpreter: bool) -> u32 {
        0
    }
}

#[test]
fn tier1_code_version_guard_ignores_trailing_invalid_page() {
    // Place an executed instruction at the last byte of a page, followed by an unsupported opcode
    // on the next page:
    //   0x0FFF: push rbx  (1 byte, executed)
    //   0x1000: nop       (unsupported by Tier1, causes Invalid terminator; not executed)
    //
    // `Tier1Compilation::byte_len` is expected to cover only executed bytes, so the page-version
    // snapshot should not include the second page. Modifying bytes in that second page should not
    // invalidate the cached block.
    let entry = 0x0fff_u64;
    let mut bus = SimpleBus::new(0x3000);
    bus.load(entry, &[0x53]); // push rbx
    bus.load(0x1000, &[0x90]); // nop (unsupported by Tier-1)

    let queue = Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let mut jit: JitRuntime<DummyBackend, Tier1CompileQueue> =
        JitRuntime::new(config, DummyBackend::default(), queue.clone());

    let mut compiler = Tier1Compiler::new(bus.clone(), NoopRegistry).with_limits(BlockLimits {
        max_insts: 16,
        max_bytes: 64,
    });
    let handle = compiler
        .compile_handle(&jit, entry, 64)
        .expect("Tier-1 compile_handle");

    assert_eq!(handle.meta.byte_len, 1);
    assert_eq!(handle.meta.page_versions.len(), 1);
    assert_eq!(handle.meta.page_versions[0].page, 0);

    jit.install_handle(handle);
    assert!(queue.is_empty());

    // Modify the next page (contains the unsupported instruction). This should NOT invalidate.
    jit.on_guest_write(0x1000, 1);
    assert!(jit.prepare_block(entry).is_some());
    assert!(queue.is_empty());

    // Modifying an executed byte must invalidate and request recompilation.
    jit.on_guest_write(entry, 1);
    assert!(jit.prepare_block(entry).is_none());
    assert_eq!(queue.drain(), vec![entry]);
}
