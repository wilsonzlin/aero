#![cfg(not(target_arch = "wasm32"))]

mod tier1_common;

use std::collections::HashMap;

use aero_cpu_core::jit::runtime::{
    JitBackend, JitBlockExit, JitConfig, JitRuntime, DEFAULT_CODE_VERSION_MAX_PAGES,
};
use aero_jit_x86::tier1_pipeline::{Tier1CompileQueue, Tier1Compiler, Tier1WasmRegistry};
use aero_jit_x86::BlockLimits;
use aero_jit_x86::Tier1Bus;
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

#[derive(Clone, Debug, Default)]
struct MapBus {
    mem: HashMap<u64, u8>,
}

impl Tier1Bus for MapBus {
    fn read_u8(&self, addr: u64) -> u8 {
        *self.mem.get(&addr).unwrap_or(&0)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.mem.insert(addr, value);
    }
}

#[test]
fn tier1_code_version_guard_ignores_trailing_invalid_page() {
    // Place an executed instruction at the last byte of a page, followed by an unsupported opcode
    // on the next page:
    //   0x0FFF: push rbx          (1 byte, executed)
    //   0x1000: <unsupported op>  (decoded as Invalid terminator; not executed)
    //
    // `Tier1Compilation::byte_len` is expected to cover only executed bytes, so the page-version
    // snapshot should not include the second page. Modifying bytes in that second page should not
    // invalidate the cached block.
    for bitness in [16u32, 32, 64] {
        let entry = 0x0fff_u64;
        let mut bus = SimpleBus::new(0x3000);
        bus.load(entry, &[0x53]); // push rbx/bx/ebx
        let invalid = tier1_common::pick_invalid_opcode(bitness);
        bus.load(0x1000, &[invalid]); // invalid/unsupported opcode (decoded as Invalid by Tier-1)

        let queue = Tier1CompileQueue::new();
        let config = JitConfig {
            enabled: true,
            hot_threshold: 1,
            cache_max_blocks: 16,
            cache_max_bytes: 0,
            code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
        };
        let mut jit: JitRuntime<DummyBackend, Tier1CompileQueue> =
            JitRuntime::new(config, DummyBackend, queue.clone());

        let mut compiler = Tier1Compiler::new(bus.clone(), NoopRegistry).with_limits(BlockLimits {
            max_insts: 16,
            max_bytes: 64,
        });
        let handle = compiler
            .compile_handle(&jit, entry, bitness)
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
        assert_eq!(
            queue.drain(),
            vec![entry],
            "expected invalidation/recompile request for bitness={bitness}"
        );
    }
}

#[test]
fn tier1_code_version_guard_wraps_ip_across_bitness_boundary() {
    // Similar to the Tier-2 wraparound regression tests, but exercising the Tier-1 compilation
    // pipeline metadata: a block whose executed bytes span the 16-bit/32-bit IP wrap boundary must
    // snapshot the wrapped page (page 0), otherwise self-modifying writes to those wrapped bytes
    // would not invalidate the cached block.
    //
    // Guest bytes:
    //   <boundary-2>: 31 C0    xor (e)ax, (e)ax   (2 bytes)
    //   0x0000:      40       inc (e)ax          (1 byte)
    //   0x0001:      <invalid>
    for (bitness, entry) in [(16u32, 0xfffeu64), (32u32, 0xffff_fffeu64)] {
        let mut bus = MapBus::default();
        bus.write_u8(entry, 0x31);
        bus.write_u8(entry.wrapping_add(1), 0xc0);
        bus.write_u8(0x0, 0x40);
        bus.write_u8(0x1, tier1_common::pick_invalid_opcode(bitness));

        let queue = Tier1CompileQueue::new();
        let config = JitConfig {
            enabled: true,
            hot_threshold: 1,
            cache_max_blocks: 16,
            cache_max_bytes: 0,
            code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
        };
        let mut jit: JitRuntime<DummyBackend, Tier1CompileQueue> =
            JitRuntime::new(config, DummyBackend, queue.clone());

        let mut compiler = Tier1Compiler::new(bus.clone(), NoopRegistry).with_limits(BlockLimits {
            max_insts: 16,
            max_bytes: 64,
        });
        let handle = compiler
            .compile_handle(&jit, entry, bitness)
            .expect("Tier-1 compile_handle");

        assert_eq!(handle.meta.byte_len, 3);
        let mut pages: Vec<u64> = handle.meta.page_versions.iter().map(|s| s.page).collect();
        pages.sort_unstable();
        let high_page = entry >> aero_jit_x86::PAGE_SHIFT;
        assert_eq!(
            pages,
            vec![0, high_page],
            "unexpected guarded pages for bitness={bitness}"
        );

        jit.install_handle(handle);
        assert!(queue.is_empty());

        // Modify the wrapped page (contains the INC instruction). This must invalidate the cached
        // block and request recompilation.
        jit.on_guest_write(0x0, 1);
        assert!(jit.prepare_block(entry).is_none());
        assert_eq!(
            queue.drain(),
            vec![entry],
            "expected invalidation for bitness={bitness}"
        );
    }
}
