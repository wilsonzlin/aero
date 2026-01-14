#![cfg(not(target_arch = "wasm32"))]

use std::cell::Cell;
use std::rc::Rc;

use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome,
};
use aero_cpu_core::jit::cache::CompiledBlockHandle;
#[cfg(feature = "tier1-inline-tlb")]
use aero_cpu_core::jit::runtime::JitBackend;
use aero_cpu_core::jit::runtime::{CompileRequestSink, JitConfig, JitRuntime, DEFAULT_CODE_VERSION_MAX_PAGES};
use aero_cpu_core::state::CpuState;
use aero_jit_x86::backend::{Tier1Cpu, WasmtimeBackend};
#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::jit_ctx;
use aero_jit_x86::tier1::ir::{BinOp, GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::tier1::Tier1WasmOptions;
#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::Tier1Bus;
use aero_types::{FlagSet, Gpr, Width};

#[derive(Default)]
struct NullCompileSink;

impl CompileRequestSink for NullCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {
        // Tests install pre-compiled blocks directly.
    }
}

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

struct TestInterpreter {
    calls: Rc<Cell<u32>>,
}

impl Interpreter<TestCpu> for TestInterpreter {
    fn exec_block(&mut self, cpu: &mut TestCpu) -> InterpreterBlockExit {
        self.calls.set(self.calls.get() + 1);
        cpu.state.gpr[Gpr::Rcx.as_u8() as usize] = 0x99;
        InterpreterBlockExit {
            next_rip: 0x4000,
            instructions_retired: 1,
        }
    }
}

fn compile_tier1_block(builder: IrBuilder, term: IrTerminator) -> Vec<u8> {
    let block = builder.finish(term);
    Tier1WasmCodegen::new().compile_block(&block)
}

#[test]
fn wasmtime_backend_executes_blocks_via_exec_dispatcher() {
    // Block 1: increment RAX, then jump to block 2.
    let entry1 = 0x1000u64;
    let entry2 = 0x2000u64;
    let entry3 = 0x3000u64;
    let entry4 = 0x5000u64;

    let mut b1 = IrBuilder::new(entry1);
    let rax = b1.read_reg(GuestReg::Gpr {
        reg: Gpr::Rax,
        width: Width::W64,
        high8: false,
    });
    let one = b1.const_int(Width::W64, 1);
    let res = b1.binop(BinOp::Add, Width::W64, rax, one, FlagSet::EMPTY);
    b1.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W64,
            high8: false,
        },
        res,
    );
    let wasm1 = compile_tier1_block(b1, IrTerminator::Jump { target: entry2 });

    // Block 2: set RBX, then request an interpreter step at entry3.
    let mut b2 = IrBuilder::new(entry2);
    let v = b2.const_int(Width::W64, 0x42);
    b2.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v,
    );
    let wasm2 = compile_tier1_block(b2, IrTerminator::ExitToInterpreter { next_rip: entry3 });

    // Block 3: would run at entry3, but block 2 requests an interpreter step so this must not
    // execute.
    let mut b3 = IrBuilder::new(entry3);
    let v = b3.const_int(Width::W64, 0xdead);
    b3.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rcx,
            width: Width::W64,
            high8: false,
        },
        v,
    );
    let wasm3 = compile_tier1_block(b3, IrTerminator::Jump { target: entry4 });

    // Build backend + runtime.
    let mut backend: WasmtimeBackend<TestCpu> = WasmtimeBackend::new();
    let idx1 = backend.add_compiled_block(&wasm1);
    let idx2 = backend.add_compiled_block(&wasm2);
    let idx3 = backend.add_compiled_block(&wasm3);

    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };
    let jit = JitRuntime::new(config, backend, NullCompileSink);
    let calls = Rc::new(Cell::new(0));
    let interpreter = TestInterpreter {
        calls: calls.clone(),
    };
    let mut dispatcher = ExecDispatcher::new(interpreter, jit);

    // Install both compiled blocks.
    {
        let jit = dispatcher.jit_mut();
        let meta = jit.make_meta(0, 0);
        jit.install_handle(CompiledBlockHandle {
            entry_rip: entry1,
            table_index: idx1,
            meta: meta.clone(),
        });
        jit.install_handle(CompiledBlockHandle {
            entry_rip: entry2,
            table_index: idx2,
            meta,
        });
        jit.install_handle(CompiledBlockHandle {
            entry_rip: entry3,
            table_index: idx3,
            meta: jit.make_meta(0, 0),
        });
    }

    let mut cpu = TestCpu::default();
    cpu.state.rip = entry1;
    cpu.state.gpr[Gpr::Rax.as_u8() as usize] = 41;

    // Step 1: runs JIT block 1.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            entry_rip,
            next_rip,
            ..
        } => {
            assert_eq!(entry_rip, entry1);
            assert_eq!(next_rip, entry2);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(cpu.state.gpr[Gpr::Rax.as_u8() as usize], 42);
    assert_eq!(
        calls.get(),
        0,
        "interpreter should not run for normal JIT exit"
    );

    // Step 2: runs JIT block 2, which requests an interpreter step at entry3.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            entry_rip,
            next_rip,
            ..
        } => {
            assert_eq!(entry_rip, entry2);
            assert_eq!(next_rip, entry3);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(cpu.state.gpr[Gpr::Rbx.as_u8() as usize], 0x42);

    // Step 3: forced interpreter step due to exit_to_interpreter flag.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier: ExecutedTier::Interpreter,
            entry_rip,
            next_rip,
            ..
        } => {
            assert_eq!(entry_rip, entry3);
            assert_eq!(next_rip, 0x4000);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(calls.get(), 1);
    assert_eq!(cpu.state.gpr[Gpr::Rcx.as_u8() as usize], 0x99);
}

#[test]
#[cfg(feature = "tier1-inline-tlb")]
fn wasmtime_backend_executes_inline_tlb_load_store() {
    fn read_u32_le(backend: &WasmtimeBackend<CpuState>, addr: u64) -> u32 {
        let bytes = [
            backend.read_u8(addr),
            backend.read_u8(addr + 1),
            backend.read_u8(addr + 2),
            backend.read_u8(addr + 3),
        ];
        u32::from_le_bytes(bytes)
    }

    let entry = 0x1000u64;

    let mut builder = IrBuilder::new(entry);
    let addr = builder.const_int(Width::W64, 0x10);
    let value = builder.const_int(Width::W32, 0x1122_3344);
    builder.store(Width::W32, addr, value);
    let loaded = builder.load(Width::W32, addr);
    builder.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
            high8: false,
        },
        loaded,
    );
    let block = builder.finish(IrTerminator::Jump { target: 0x2000 });

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let mut backend: WasmtimeBackend<CpuState> = WasmtimeBackend::new();
    let idx = backend.add_compiled_block(&wasm);

    // The backend should configure the code-version table in linear memory.
    let cpu_ptr = WasmtimeBackend::<CpuState>::DEFAULT_CPU_PTR as u64;
    let table_ptr = read_u32_le(
        &backend,
        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as u64,
    ) as u64;
    let table_len = read_u32_le(
        &backend,
        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as u64,
    ) as u64;
    assert_ne!(
        table_len, 0,
        "WasmtimeBackend should configure a non-empty code version table when it fits"
    );
    assert!(
        table_len <= DEFAULT_CODE_VERSION_MAX_PAGES as u64,
        "table_len should be clamped to DEFAULT_CODE_VERSION_MAX_PAGES"
    );
    let min_pages =
        (cpu_ptr + aero_jit_x86::PAGE_SIZE.saturating_sub(1)) / aero_jit_x86::PAGE_SIZE;
    assert!(
        table_len >= min_pages,
        "table_len should cover the guest RAM window (0..cpu_ptr): len={} min={}",
        table_len,
        min_pages
    );
    assert_ne!(
        table_ptr, 0,
        "table_ptr should be non-zero when table_len is non-zero"
    );
    assert_eq!(table_ptr % 4, 0, "table_ptr must be 4-byte aligned");

    // Sanity-check that the last entry is readable (ptr/len are in-bounds).
    let last_entry_off = table_ptr + (table_len - 1) * 4;
    let _ = read_u32_le(&backend, last_entry_off);

    // The inline-TLB store should bump the version for page 0.
    let page = 0x10u64 >> aero_jit_x86::PAGE_SHIFT;
    assert_eq!(page, 0);
    let entry_off = table_ptr + page * 4;
    assert_eq!(read_u32_le(&backend, entry_off), 0);

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };

    let exit = backend.execute(idx, &mut cpu);
    assert_eq!(exit.next_rip, 0x2000);
    assert!(!exit.exit_to_interpreter);
    assert!(exit.committed);
    assert_eq!(cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1122_3344);
    assert_eq!(
        read_u32_le(&backend, entry_off),
        1,
        "inline-TLB store should bump the code version for the touched page"
    );

    let bytes = [
        backend.read_u8(0x10),
        backend.read_u8(0x11),
        backend.read_u8(0x12),
        backend.read_u8(0x13),
    ];
    assert_eq!(u32::from_le_bytes(bytes), 0x1122_3344);

    // Slow-path stores (env.mem_write_u*) should also bump versions via the backend helper.
    let entry2 = 0x3000u64;
    let mut b2 = IrBuilder::new(entry2);
    let addr2 = b2.const_int(Width::W64, 0x1000);
    let value2 = b2.const_int(Width::W8, 0x7f);
    b2.store(Width::W8, addr2, value2);
    let block2 = b2.finish(IrTerminator::Jump { target: 0x4000 });
    let wasm2 = Tier1WasmCodegen::new().compile_block_with_options(
        &block2,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_stores: false,
            ..Default::default()
        },
    );
    let idx2 = backend.add_compiled_block(&wasm2);

    let page2 = 0x1000u64 >> aero_jit_x86::PAGE_SHIFT;
    assert_eq!(page2, 1);
    let entry2_off = table_ptr + page2 * 4;
    assert_eq!(read_u32_le(&backend, entry2_off), 0);

    let mut cpu2 = CpuState {
        rip: entry2,
        ..Default::default()
    };
    let exit2 = backend.execute(idx2, &mut cpu2);
    assert_eq!(exit2.next_rip, 0x4000);
    assert!(!exit2.exit_to_interpreter);
    assert!(exit2.committed);
    assert_eq!(
        backend.read_u8(0x1000),
        0x7f,
        "slow-path store should write through the imported helper"
    );
    assert_eq!(
        read_u32_le(&backend, entry2_off),
        1,
        "slow-path mem_write should bump the code version for the touched page"
    );
}

#[test]
#[cfg(feature = "tier1-inline-tlb")]
fn wasmtime_backend_disables_code_version_table_when_out_of_memory() {
    fn read_u32_le(backend: &WasmtimeBackend<CpuState>, addr: u64) -> u32 {
        let bytes = [
            backend.read_u8(addr),
            backend.read_u8(addr + 1),
            backend.read_u8(addr + 2),
            backend.read_u8(addr + 3),
        ];
        u32::from_le_bytes(bytes)
    }

    // Construct a backend where the CPU/JIT context fits, but there is no room left for even the
    // smallest page-version table allocation. In this case the backend should disable the table
    // by writing `LEN=0` (and `PTR=0`) into the Tier-2 ABI slots.
    let memory_pages = 2u32;
    let byte_len = u64::from(memory_pages) * 65_536;
    let reserved = u64::from(jit_ctx::TIER2_CTX_OFFSET + jit_ctx::TIER2_CTX_SIZE);
    assert!(
        reserved < byte_len,
        "test invariant: CPU/JIT context must fit in the chosen memory size"
    );
    let cpu_ptr = (byte_len - reserved) as i32;

    let entry = 0x1000u64;
    let mut builder = IrBuilder::new(entry);
    let addr = builder.const_int(Width::W64, 0x10);
    let value = builder.const_int(Width::W32, 0x1122_3344);
    builder.store(Width::W32, addr, value);
    let loaded = builder.load(Width::W32, addr);
    builder.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
            high8: false,
        },
        loaded,
    );
    let block = builder.finish(IrTerminator::Jump { target: 0x2000 });
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let mut backend: WasmtimeBackend<CpuState> = WasmtimeBackend::new_with_memory_pages(
        memory_pages,
        cpu_ptr,
    );
    let idx = backend.add_compiled_block(&wasm);

    let cpu_ptr_u64 = cpu_ptr as u64;
    let table_ptr = read_u32_le(
        &backend,
        cpu_ptr_u64 + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as u64,
    );
    let table_len = read_u32_le(
        &backend,
        cpu_ptr_u64 + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as u64,
    );
    assert_eq!(table_ptr, 0);
    assert_eq!(table_len, 0);

    // The block should still execute correctly; it just won't bump versions.
    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    let exit = backend.execute(idx, &mut cpu);
    assert_eq!(exit.next_rip, 0x2000);
    assert!(!exit.exit_to_interpreter);
    assert!(exit.committed);
    assert_eq!(cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1122_3344);
}

#[test]
#[cfg(feature = "tier1-inline-tlb")]
fn wasmtime_backend_mmio_exit_rolls_back_code_version_bumps() {
    fn read_u32_le(backend: &WasmtimeBackend<CpuState>, addr: u64) -> u32 {
        let bytes = [
            backend.read_u8(addr),
            backend.read_u8(addr + 1),
            backend.read_u8(addr + 2),
            backend.read_u8(addr + 3),
        ];
        u32::from_le_bytes(bytes)
    }

    let entry = 0x1000u64;
    let mmio_addr = (WasmtimeBackend::<CpuState>::DEFAULT_CPU_PTR as u64).saturating_add(0x1000);

    // Store to RAM (should bump code versions), then trigger an MMIO load which will cause the
    // backend to roll back the entire block.
    let mut builder = IrBuilder::new(entry);
    let ram_addr = builder.const_int(Width::W64, 0x10);
    let store_value = builder.const_int(Width::W8, 0xaa);
    builder.store(Width::W8, ram_addr, store_value);
    let mmio_addr = builder.const_int(Width::W64, mmio_addr);
    let loaded = builder.load(Width::W8, mmio_addr);
    builder.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W8,
            high8: false,
        },
        loaded,
    );
    let block = builder.finish(IrTerminator::Jump { target: 0x2000 });

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let mut backend: WasmtimeBackend<CpuState> = WasmtimeBackend::new();
    let idx = backend.add_compiled_block(&wasm);

    // Inspect table ptr/len.
    let cpu_ptr = WasmtimeBackend::<CpuState>::DEFAULT_CPU_PTR as u64;
    let table_ptr = read_u32_le(
        &backend,
        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as u64,
    ) as u64;
    let table_len = read_u32_le(
        &backend,
        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as u64,
    ) as u64;
    assert_ne!(table_len, 0);
    assert_ne!(table_ptr, 0);
    let entry_off = table_ptr; // page 0
    assert_eq!(read_u32_le(&backend, entry_off), 0);

    // Memory starts zeroed.
    assert_eq!(backend.read_u8(0x10), 0);

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    let exit = backend.execute(idx, &mut cpu);

    assert!(exit.exit_to_interpreter);
    assert_eq!(exit.next_rip, entry);
    assert!(!exit.committed);

    // The store should have been rolled back, and so should the code version bump.
    assert_eq!(backend.read_u8(0x10), 0);
    assert_eq!(
        read_u32_le(&backend, entry_off),
        0,
        "code-version bump must be rolled back when the block does not commit"
    );
}

#[test]
#[cfg(feature = "tier1-inline-tlb")]
fn wasmtime_backend_inline_tlb_mmio_exit_sets_next_rip() {
    let entry = 0x1000u64;

    // Load from an address outside the guest RAM window (0..cpu_ptr). The reference backend
    // classifies this as MMIO and should request an interpreter step via the sentinel ABI.
    let mmio_addr = (WasmtimeBackend::<CpuState>::DEFAULT_CPU_PTR as u64).saturating_add(0x1000);

    let mut builder = IrBuilder::new(entry);
    let addr = builder.const_int(Width::W64, mmio_addr);
    let loaded = builder.load(Width::W8, addr);
    builder.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W8,
            high8: false,
        },
        loaded,
    );
    let block = builder.finish(IrTerminator::Jump { target: 0x2000 });

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let mut backend: WasmtimeBackend<CpuState> = WasmtimeBackend::new();
    let idx = backend.add_compiled_block(&wasm);

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };

    let exit = backend.execute(idx, &mut cpu);
    assert!(exit.exit_to_interpreter);
    assert_eq!(exit.next_rip, entry);
    assert!(!exit.committed, "MMIO exit rolls back guest state");
    assert_eq!(cpu.rip, entry);
}
