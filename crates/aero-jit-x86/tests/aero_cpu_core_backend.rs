#![cfg(not(target_arch = "wasm32"))]

use std::cell::Cell;
use std::rc::Rc;

use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome,
};
use aero_cpu_core::jit::cache::CompiledBlockHandle;
#[cfg(feature = "tier1-inline-tlb")]
use aero_cpu_core::jit::runtime::JitBackend;
use aero_cpu_core::jit::runtime::{CompileRequestSink, JitConfig, JitRuntime};
use aero_cpu_core::state::CpuState;
use aero_jit_x86::backend::{Tier1Cpu, WasmtimeBackend};
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

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };

    let exit = backend.execute(idx, &mut cpu);
    assert_eq!(exit.next_rip, 0x2000);
    assert!(!exit.exit_to_interpreter);
    assert!(exit.committed);
    assert_eq!(cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1122_3344);

    let bytes = [
        backend.read_u8(0x10),
        backend.read_u8(0x11),
        backend.read_u8(0x12),
        backend.read_u8(0x13),
    ];
    assert_eq!(u32::from_le_bytes(bytes), 0x1122_3344);
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
