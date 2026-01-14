#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::rc::Rc;

use aero_cpu_core::jit::cache::CompiledBlockHandle;
use aero_cpu_core::jit::runtime::{JitBackend, JitConfig, JitRuntime, DEFAULT_CODE_VERSION_MAX_PAGES};
use aero_cpu_core::state::CpuState;
use aero_jit_x86::backend::{WasmBackend, WasmtimeBackend, WriteObserver};
use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
use aero_types::Width;

#[test]
fn wasmtime_backend_write_observer_reports_tier1_store_addresses_and_lengths() {
    let entry_rip = 0x1000u64;

    // Emit a block that performs 4 stores of different widths using the slow-path imported helpers
    // (`env.mem_write_*`).
    let mut b = IrBuilder::new(entry_rip);
    let a8 = b.const_int(Width::W64, 0x10);
    let v8 = b.const_int(Width::W8, 0xAA);
    b.store(Width::W8, a8, v8);

    let a16 = b.const_int(Width::W64, 0x20);
    let v16 = b.const_int(Width::W16, 0xBEEF);
    b.store(Width::W16, a16, v16);

    let a32 = b.const_int(Width::W64, 0x30);
    let v32 = b.const_int(Width::W32, 0xDEAD_BEEF);
    b.store(Width::W32, a32, v32);

    let a64 = b.const_int(Width::W64, 0x40);
    let v64 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a64, v64);

    let block = b.finish(IrTerminator::Jump { target: 0x2000 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);

    let writes: Rc<RefCell<Vec<(u64, usize)>>> = Rc::new(RefCell::new(Vec::new()));
    let observer: WriteObserver = Box::new({
        let writes = writes.clone();
        move |paddr, len| writes.borrow_mut().push((paddr, len))
    });

    let mut backend: WasmtimeBackend<CpuState> = WasmtimeBackend::new();
    backend.set_write_observer(Some(observer));
    let idx = backend.add_compiled_block(&wasm);

    let mut cpu = CpuState {
        rip: entry_rip,
        ..Default::default()
    };
    let exit = backend.execute(idx, &mut cpu);
    assert_eq!(exit.next_rip, 0x2000);

    assert_eq!(
        *writes.borrow(),
        vec![(0x10, 1), (0x20, 2), (0x30, 4), (0x40, 8)]
    );
}

#[test]
fn wasmtime_backend_write_observer_can_keep_jit_runtime_page_versions_coherent() {
    let entry_rip = 0x1000u64;
    let writer_rip = 0x2000u64;

    let mut backend: WasmBackend<CpuState> = WasmBackend::new();

    // Construct a runtime and install a compiled block with a non-empty page-version snapshot.
    let compile_queue = aero_jit_x86::tier1_pipeline::Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        // Keep the block from becoming hot; we only care about stale invalidation.
        hot_threshold: 1_000_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };
    let jit = Rc::new(RefCell::new(JitRuntime::new(
        config,
        backend.clone(),
        compile_queue.clone(),
    )));

    // Wire the backend's imported write helpers into the runtime's page-version tracker.
    backend.set_write_observer(Some(Box::new({
        let jit = jit.clone();
        move |paddr, len| jit.borrow_mut().on_guest_write(paddr, len)
    })));

    // Target block (the one we'll invalidate by writing to its code page).
    let tb = IrBuilder::new(entry_rip);
    let target_block = tb.finish(IrTerminator::Jump { target: 0x3000 });
    target_block.validate().unwrap();
    let target_wasm = Tier1WasmCodegen::new().compile_block(&target_block);
    let target_idx = backend.add_compiled_block(&target_wasm);

    // Install into the runtime with a non-empty snapshot.
    {
        let mut jit = jit.borrow_mut();
        let meta = jit.snapshot_meta(entry_rip, 1);
        jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index: target_idx,
            meta,
        });
        assert!(jit.is_compiled(entry_rip));
        assert!(jit.prepare_block(entry_rip).is_some());
    }
    assert!(compile_queue.is_empty());

    // Writer block: self-modify the code page of `entry_rip` using a Tier-1 store. This should
    // trigger the write observer and bump the runtime's page-version tracker.
    let mut wb = IrBuilder::new(writer_rip);
    let addr = wb.const_int(Width::W64, entry_rip);
    let value = wb.const_int(Width::W8, 0x90);
    wb.store(Width::W8, addr, value);
    let writer_block = wb.finish(IrTerminator::Jump { target: 0x4000 });
    writer_block.validate().unwrap();
    let writer_wasm = Tier1WasmCodegen::new().compile_block(&writer_block);
    let writer_idx = backend.add_compiled_block(&writer_wasm);

    let mut cpu = CpuState {
        rip: writer_rip,
        ..Default::default()
    };
    let _exit = backend.execute(writer_idx, &mut cpu);

    // The runtime should now treat the previously-installed block as stale and request
    // recompilation, without any explicit `jit.on_guest_write(..)` calls in the test itself.
    {
        let mut jit = jit.borrow_mut();
        assert!(jit.prepare_block(entry_rip).is_none());
        assert!(!jit.is_compiled(entry_rip));
    }
    assert_eq!(compile_queue.drain(), vec![entry_rip]);
}
