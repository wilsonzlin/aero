use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use aero_cpu::{CpuBus, CpuState};
use aero_cpu_core::exec::{ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, StepOutcome};
use aero_cpu_core::jit::runtime::{JitBackend, JitBlockExit, JitConfig, JitRuntime};
use aero_jit::tier1_ir::interp as tier1_interp;
use aero_jit::{discover_block, BlockLimits, Tier1CompileQueue, Tier1Compiler, Tier1WasmRegistry};
use aero_types::Gpr;
use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const CPU_PTR: i32 = 0x1_0000;

#[derive(Clone, Debug)]
struct SharedMem(Rc<RefCell<Vec<u8>>>);

impl SharedMem {
    fn new(size: usize) -> Self {
        Self(Rc::new(RefCell::new(vec![0; size])))
    }

    fn load(&self, addr: u64, bytes: &[u8]) {
        let start = addr as usize;
        let end = start + bytes.len();
        self.0.borrow_mut()[start..end].copy_from_slice(bytes);
    }
}

impl CpuBus for SharedMem {
    fn read_u8(&self, addr: u64) -> u8 {
        self.0.borrow()[addr as usize]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.0.borrow_mut()[addr as usize] = value;
    }
}

#[derive(Debug, Clone)]
struct TestCpu {
    state: CpuState,
    mem: SharedMem,
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
    mem: SharedMem,
}

impl Interpreter<TestCpu> for Tier1Interpreter {
    fn exec_block(&mut self, cpu: &mut TestCpu) -> u64 {
        let entry = cpu.rip();
        let block = discover_block(&self.mem, entry, BlockLimits::default());
        let ir = aero_jit::translate_block(&block);
        let mut cpu_mem = vec![0u8; aero_jit::abi::CPU_STATE_SIZE as usize];
        tier1_interp::TestCpu {
            gpr: cpu.state.gpr,
            rip: cpu.state.rip,
            rflags: cpu.state.rflags,
        }
        .write_to_abi_mem(&mut cpu_mem, 0);

        let _ = tier1_interp::execute_block(&ir, &mut cpu_mem, &mut self.mem);
        let out = tier1_interp::TestCpu::from_abi_mem(&cpu_mem, 0);
        cpu.state.gpr = out.gpr;
        cpu.state.rip = out.rip;
        cpu.state.rflags = out.rflags;
        cpu.state.rip
    }
}

#[derive(Clone)]
struct SharedWasmiBackend(Rc<RefCell<WasmiBackendInner>>);

impl SharedWasmiBackend {
    fn new(mem_len: usize) -> Self {
        Self(Rc::new(RefCell::new(WasmiBackendInner::new(mem_len))))
    }
}

impl Tier1WasmRegistry for SharedWasmiBackend {
    fn register_tier1_block(&mut self, wasm: Vec<u8>, exit_to_interpreter: bool) -> u32 {
        self.0
            .borrow_mut()
            .register_tier1_block(wasm, exit_to_interpreter)
    }
}

impl JitBackend for SharedWasmiBackend {
    type Cpu = TestCpu;

    fn execute(&mut self, table_index: u32, cpu: &mut TestCpu) -> JitBlockExit {
        self.0.borrow_mut().execute(table_index, cpu)
    }
}

struct WasmiBlockEntry {
    func: TypedFunc<i32, i64>,
    exit_to_interpreter: bool,
}

struct WasmiBackendInner {
    engine: Engine,
    store: Store<()>,
    linker: Linker<()>,
    memory: Memory,
    blocks: HashMap<u32, WasmiBlockEntry>,
    next_table_index: u32,
    mem_len: usize,
}

impl WasmiBackendInner {
    fn new(mem_len: usize) -> Self {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new(&engine);

        // Page 0: guest memory. Page 1: CpuState at CPU_PTR.
        let memory = Memory::new(&mut store, MemoryType::new(2, None)).unwrap();
        linker
            .define(
                aero_jit::wasm::IMPORT_MODULE,
                aero_jit::wasm::IMPORT_MEMORY,
                memory.clone(),
            )
            .unwrap();

        define_mem_helpers(&mut store, &mut linker, memory.clone());

        linker
            .define(
                aero_jit::wasm::IMPORT_MODULE,
                aero_jit::wasm::IMPORT_PAGE_FAULT,
                Func::wrap(
                    &mut store,
                    |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 {
                        panic!("page_fault should not be called by tier1 runtime test");
                    },
                ),
            )
            .unwrap();

        linker
            .define(
                aero_jit::wasm::IMPORT_MODULE,
                aero_jit::wasm::IMPORT_JIT_EXIT,
                Func::wrap(
                    &mut store,
                    |_caller: Caller<'_, ()>, _kind: i32, _rip: i64| -> i64 {
                        aero_jit::wasm::JIT_EXIT_SENTINEL_I64
                    },
                ),
            )
            .unwrap();

        Self {
            engine,
            store,
            linker,
            memory,
            blocks: HashMap::new(),
            next_table_index: 0,
            mem_len,
        }
    }

    fn register_tier1_block(&mut self, wasm: Vec<u8>, exit_to_interpreter: bool) -> u32 {
        let module = Module::new(&self.engine, wasm).unwrap();
        let instance = self
            .linker
            .instantiate_and_start(&mut self.store, &module)
            .unwrap();
        let func = instance
            .get_typed_func::<i32, i64>(&self.store, aero_jit::wasm::EXPORT_BLOCK_FN)
            .unwrap();

        let table_index = self.next_table_index;
        self.next_table_index += 1;
        self.blocks.insert(
            table_index,
            WasmiBlockEntry {
                func,
                exit_to_interpreter,
            },
        );
        table_index
    }

    fn execute(&mut self, table_index: u32, cpu: &mut TestCpu) -> JitBlockExit {
        let entry = self
            .blocks
            .get(&table_index)
            .unwrap_or_else(|| panic!("missing table entry {table_index}"));
        let func = entry.func;
        let exit_to_interpreter = entry.exit_to_interpreter;

        // Sync guest memory into the wasm linear memory.
        {
            let mem = cpu.mem.0.borrow();
            assert_eq!(
                mem.len(),
                self.mem_len,
                "test assumes fixed guest memory length"
            );
            self.memory.write(&mut self.store, 0, &mem).unwrap();
        }

        // Write CpuState.
        let mut cpu_bytes = vec![0u8; aero_jit::abi::CPU_STATE_SIZE as usize];
        tier1_interp::TestCpu {
            gpr: cpu.state.gpr,
            rip: cpu.state.rip,
            rflags: cpu.state.rflags,
        }
        .write_to_abi_mem(&mut cpu_bytes, 0);
        self.memory
            .write(&mut self.store, CPU_PTR as usize, &cpu_bytes)
            .unwrap();

        let ret = func.call(&mut self.store, CPU_PTR).unwrap();

        // Read back guest memory.
        {
            let mut out_mem = vec![0u8; self.mem_len];
            self.memory.read(&self.store, 0, &mut out_mem).unwrap();
            cpu.mem.0.borrow_mut().copy_from_slice(&out_mem);
        }

        // Read back CpuState.
        let mut out_cpu_bytes = vec![0u8; aero_jit::abi::CPU_STATE_SIZE as usize];
        self.memory
            .read(&self.store, CPU_PTR as usize, &mut out_cpu_bytes)
            .unwrap();
        let out = tier1_interp::TestCpu::from_abi_mem(&out_cpu_bytes, 0);
        cpu.state.gpr = out.gpr;
        cpu.state.rip = out.rip;
        cpu.state.rflags = out.rflags;

        let exit_to_interpreter = exit_to_interpreter || ret == aero_jit::wasm::JIT_EXIT_SENTINEL_I64;
        let next_rip = if ret == aero_jit::wasm::JIT_EXIT_SENTINEL_I64 {
            cpu.state.rip
        } else {
            ret as u64
        };

        JitBlockExit {
            next_rip,
            exit_to_interpreter,
        }
    }
}

fn define_mem_helpers(store: &mut Store<()>, linker: &mut Linker<()>, memory: Memory) {
    fn read<const N: usize>(caller: &mut Caller<'_, ()>, memory: &Memory, addr: usize) -> u64 {
        let mut buf = [0u8; N];
        memory
            .read(caller, addr, &mut buf)
            .expect("memory read in bounds");
        let mut v = 0u64;
        for (i, b) in buf.iter().enumerate() {
            v |= (*b as u64) << (i * 8);
        }
        v
    }

    fn write<const N: usize>(
        caller: &mut Caller<'_, ()>,
        memory: &Memory,
        addr: usize,
        value: u64,
    ) {
        let mut buf = vec![0u8; N];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (value >> (i * 8)) as u8;
        }
        memory
            .write(caller, addr, &buf)
            .expect("memory write in bounds");
    }

    let mem = memory.clone();
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_READ_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<1>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_READ_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<2>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_READ_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<4>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_READ_U64,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i64 {
                    read::<8>(&mut caller, &mem, addr as usize) as i64
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_WRITE_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<1>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_WRITE_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<2>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_WRITE_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<4>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            aero_jit::wasm::IMPORT_MODULE,
            aero_jit::wasm::IMPORT_MEM_WRITE_U64,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i64| {
                    write::<8>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();
}

fn setup_runtime(
    hot_threshold: u32,
) -> (
    SharedMem,
    SharedWasmiBackend,
    Tier1CompileQueue,
    ExecDispatcher<Tier1Interpreter, SharedWasmiBackend, Tier1CompileQueue>,
    Tier1Compiler<SharedMem, SharedWasmiBackend>,
    TestCpu,
) {
    let entry = 0x1000u64;
    let code = [
        0x83, 0xc0, 0x01, // add eax, 1
        0xeb, 0xfb, // jmp -5
    ];

    let mem = SharedMem::new(0x10000);
    mem.load(entry, &code);

    let backend = SharedWasmiBackend::new(mem.0.borrow().len());
    let compile_queue = Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        hot_threshold,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend.clone(), compile_queue.clone());
    let interp = Tier1Interpreter { mem: mem.clone() };
    let dispatcher = ExecDispatcher::new(interp, jit);

    let compiler = Tier1Compiler::new(mem.clone(), backend.clone()).with_limits(BlockLimits {
        max_insts: 16,
        max_bytes: 64,
    });

    let mut cpu = TestCpu {
        state: CpuState::default(),
        mem: mem.clone(),
    };
    cpu.state.rip = entry;

    (mem, backend, compile_queue, dispatcher, compiler, cpu)
}

#[test]
fn tier1_end_to_end_compile_install_and_execute() {
    let entry = 0x1000u64;
    let (_mem, backend, compile_queue, mut dispatcher, mut compiler, mut cpu) = setup_runtime(3);

    // Run blocks until hotness triggers compilation. Keep running to ensure the request is only
    // queued once.
    for i in 0..5 {
        match dispatcher.step(&mut cpu) {
            StepOutcome::Block {
                tier,
                entry_rip,
                next_rip,
            } => {
                assert_eq!(
                    tier,
                    ExecutedTier::Interpreter,
                    "step {i} should be interpreted"
                );
                assert_eq!(entry_rip, entry);
                assert_eq!(next_rip, entry);
            }
            StepOutcome::InterruptDelivered => panic!("unexpected interrupt"),
        }
    }

    // Hotness should trigger a single compile request for this RIP.
    assert_eq!(compile_queue.drain(), vec![entry]);

    // Compile and install Tier-1 block.
    {
        let jit = dispatcher.jit_mut();
        let handle = compiler.compile_handle(&*jit, entry).unwrap();
        jit.install_handle(handle);
    }

    // Subsequent executions should use the JIT tier and continue producing correct exits.
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
        StepOutcome::InterruptDelivered => panic!("unexpected interrupt"),
    }

    // 5 interpreted blocks + 1 JIT block, each does `add eax, 1`.
    assert_eq!(cpu.state.read_gpr(Gpr::Rax), 6);

    // Ensure the backend handle we used for compilation is shared with the runtime backend.
    // If it weren't, we'd install a table index that the runtime couldn't execute.
    let mut backend = backend;
    let _ = backend.execute(0, &mut cpu);
}

#[test]
fn tier1_stale_snapshot_is_rejected_and_requeued() {
    let entry = 0x1000u64;
    let (_mem, _backend, compile_queue, mut dispatcher, mut compiler, mut cpu) = setup_runtime(1);

    // First block execution should request compilation immediately.
    dispatcher.step(&mut cpu);
    assert_eq!(compile_queue.drain(), vec![entry]);

    // Compile, then invalidate the code page before installing the handle.
    {
        let jit = dispatcher.jit_mut();
        let handle = compiler.compile_handle(&*jit, entry).unwrap();
        jit.on_guest_write(entry, 1);
        jit.install_handle(handle);
        assert!(!jit.is_compiled(entry));
    }

    // The runtime should request compilation again after rejecting the stale handle.
    assert_eq!(compile_queue.drain(), vec![entry]);
}
