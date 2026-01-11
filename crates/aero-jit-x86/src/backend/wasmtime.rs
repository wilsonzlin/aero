use std::marker::PhantomData;

use aero_cpu_core::jit::runtime::{JitBackend, JitBlockExit};
use aero_cpu_core::state::CpuState as CoreCpuState;
use wasmtime::{Caller, Config, Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

use super::Tier1Cpu;
use crate::abi;
use crate::jit_ctx::JitContext;
use crate::tier1_pipeline::Tier1WasmRegistry;
use crate::wasm::tier1::EXPORT_TIER1_BLOCK_FN;
use crate::wasm::{
    IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32,
    IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32,
    IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
    IMPORT_PAGE_FAULT, JIT_EXIT_SENTINEL_I64,
};
use crate::Tier1Bus;

#[derive(Debug, Default, Clone, Copy)]
struct HostExitState {
    mmio_exit: bool,
    jit_exit: bool,
    page_fault: bool,
}

impl HostExitState {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn should_rollback(self) -> bool {
        self.mmio_exit || self.jit_exit || self.page_fault
    }
}

/// Reference `wasmtime`-powered backend that can execute Tier-1 compiled blocks.
///
/// ## Tier-1 ABI contract (`export block(cpu_ptr: i32, jit_ctx_ptr: i32) -> i64`)
///
/// The compiled block receives a pointer (`cpu_ptr`) into the shared linear memory where an
/// [`aero_cpu_core::state::CpuState`] is stored.
///
/// The block also receives a pointer (`jit_ctx_ptr`) to a JIT-only context region (`crate::jit_ctx`)
/// stored separately from the architectural CPU state. This allows the Tier-1 code generator to
/// use an optional inline direct-mapped TLB + direct-RAM fast-path without polluting
/// `aero_cpu_core::state::CpuState`.
///
/// The block mutates CPU state in-place and returns an `i64`:
///
/// - `ret != JIT_EXIT_SENTINEL_I64`: `ret` is the next RIP; execution can continue in the JIT.
/// - `ret == JIT_EXIT_SENTINEL_I64`: the block requests a one-shot exit to the interpreter.
///   The concrete `next_rip` is read from `CpuState.rip` after the block has updated it.
///
/// This mirrors the existing sentinel-based contract used by the older baseline WASM codegen.
pub struct WasmtimeBackend<Cpu> {
    engine: Engine,
    store: Store<HostExitState>,
    linker: Linker<HostExitState>,
    memory: Memory,
    cpu_ptr: i32,
    jit_ctx_ptr: i32,
    blocks: Vec<TypedFunc<(i32, i32), i64>>,
    _phantom: PhantomData<Cpu>,
}

impl<Cpu> WasmtimeBackend<Cpu> {
    /// Default location for the `CpuState` structure within linear memory.
    ///
    /// Page 0 is used as guest RAM; page 1 begins at 0x1_0000.
    pub const DEFAULT_CPU_PTR: i32 = 0x1_0000;

    /// Create a backend with a fixed 128KiB linear memory (two WASM pages).
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_memory_pages(2, Self::DEFAULT_CPU_PTR)
    }

    /// Create a backend with a configurable memory size and `cpu_ptr` base.
    #[must_use]
    pub fn new_with_memory_pages(memory_pages: u32, cpu_ptr: i32) -> Self {
        // Explicitly enable WebAssembly SIMD so the same backend can execute future tier-1 blocks
        // that make use of SIMD ops.
        //
        // (Wasmtime's default feature set has historically changed over time; keeping this
        // explicit makes the backend's capabilities stable across upgrades.)
        let mut config = Config::new();
        config.wasm_simd(true);
        let engine = Engine::new(&config).expect("create wasmtime engine");
        let mut store = Store::new(&engine, HostExitState::default());
        let mut linker = Linker::new(&engine);

        // A single shared memory is imported by all generated blocks.
        let memory = Memory::new(&mut store, MemoryType::new(memory_pages, None))
            .expect("create wasmtime memory");
        linker
            .define(&mut store, IMPORT_MODULE, IMPORT_MEMORY, memory)
            .expect("define env.memory import");

        define_mem_helpers(&mut linker, memory);
        define_stub_helpers(&mut linker, memory);

        // Verify the configured CpuState window fits within the linear memory.
        let byte_len = (memory_pages as usize)
            .checked_mul(65_536)
            .expect("memory_pages overflow");
        let jit_ctx_ptr = cpu_ptr
            .checked_add(abi::CPU_STATE_SIZE as i32)
            .expect("jit_ctx_ptr overflow");
        let cpu_end = (cpu_ptr as usize)
            .checked_add(abi::CPU_STATE_SIZE as usize)
            .expect("cpu_ptr overflow");
        let ctx_end = (jit_ctx_ptr as usize)
            .checked_add(JitContext::TOTAL_BYTE_SIZE)
            .expect("jit_ctx_ptr overflow");
        let end = cpu_end.max(ctx_end);
        assert!(
            end <= byte_len,
            "cpu_ptr (0x{cpu_ptr:x}) + cpu/jit_ctx regions (end=0x{end:x}) must fit in linear memory ({} bytes)",
            byte_len
        );

        Self {
            engine,
            store,
            linker,
            memory,
            cpu_ptr,
            jit_ctx_ptr,
            blocks: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Instantiate a Tier-1 block WASM module and append it to the internal table.
    ///
    /// Returns the table index used by `JitRuntime` and [`Self::execute`].
    pub fn add_compiled_block(&mut self, wasm_bytes: &[u8]) -> u32 {
        let module = Module::new(&self.engine, wasm_bytes).expect("compile wasm module");
        let instance = self
            .linker
            .instantiate(&mut self.store, &module)
            .expect("instantiate wasm module");
        let func = instance
            .get_typed_func::<(i32, i32), i64>(&mut self.store, EXPORT_TIER1_BLOCK_FN)
            .expect("get exported tier1 block function");
        let idx = self.blocks.len() as u32;
        self.blocks.push(func);
        idx
    }

    fn sync_cpu_to_wasm(&mut self, cpu: &CoreCpuState) {
        for i in 0..16 {
            let off = self.cpu_ptr as usize + abi::CPU_GPR_OFF[i] as usize;
            self.memory
                .write(&mut self.store, off, &cpu.gpr[i].to_le_bytes())
                .expect("write CpuState.gpr into linear memory");
        }
        let rip_off = self.cpu_ptr as usize + abi::CPU_RIP_OFF as usize;
        self.memory
            .write(&mut self.store, rip_off, &cpu.rip.to_le_bytes())
            .expect("write CpuState.rip into linear memory");

        let rflags_off = self.cpu_ptr as usize + abi::CPU_RFLAGS_OFF as usize;
        let rflags = cpu.rflags_snapshot();
        self.memory
            .write(&mut self.store, rflags_off, &rflags.to_le_bytes())
            .expect("write CpuState.rflags into linear memory");

        // Keep the Tier-1 JIT context header initialized for the optional inline-TLB fast-path.
        let mem = self.memory.data_mut(&mut self.store);
        let ctx = JitContext {
            ram_base: 0, // guest RAM begins at linear address 0
            tlb_salt: 0x1234_5678_9abc_def0,
        };
        ctx.write_header_to_mem(mem, self.jit_ctx_ptr as usize);
    }

    fn sync_cpu_from_wasm(&mut self, cpu: &mut CoreCpuState) {
        let mut buf = vec![0u8; abi::CPU_STATE_SIZE as usize];
        self.memory
            .read(&self.store, self.cpu_ptr as usize, &mut buf)
            .expect("read CpuState from linear memory");

        for i in 0..16 {
            let off = abi::CPU_GPR_OFF[i] as usize;
            let mut b = [0u8; 8];
            b.copy_from_slice(&buf[off..off + 8]);
            cpu.gpr[i] = u64::from_le_bytes(b);
        }

        let mut b = [0u8; 8];
        let rip_off = abi::CPU_RIP_OFF as usize;
        b.copy_from_slice(&buf[rip_off..rip_off + 8]);
        cpu.rip = u64::from_le_bytes(b);

        let rflags_off = abi::CPU_RFLAGS_OFF as usize;
        b.copy_from_slice(&buf[rflags_off..rflags_off + 8]);
        cpu.set_rflags(u64::from_le_bytes(b));
    }
}

impl<Cpu> Tier1Bus for WasmtimeBackend<Cpu> {
    fn read_u8(&self, addr: u64) -> u8 {
        self.memory.data(&self.store)[addr as usize]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.memory.data_mut(&mut self.store)[addr as usize] = value;
    }
}

impl<Cpu> JitBackend for WasmtimeBackend<Cpu>
where
    Cpu: Tier1Cpu,
{
    type Cpu = Cpu;

    fn execute(&mut self, table_index: u32, cpu: &mut Self::Cpu) -> JitBlockExit {
        let func = self
            .blocks
            .get(table_index as usize)
            .cloned()
            .unwrap_or_else(|| panic!("invalid JIT table index {table_index}"));

        // Snapshot state so we can roll back side effects if the block performs an MMIO/runtime
        // exit. Tier-1 blocks do not currently provide deopt metadata for resuming mid-block, so
        // the conservative fallback is to roll back and let the interpreter re-execute.
        let pre_state = cpu.tier1_state().clone();
        let ram_len = usize::try_from(self.cpu_ptr).expect("cpu_ptr must be non-negative");
        let pre_ram = self.memory.data(&self.store)[..ram_len].to_vec();

        self.store.data_mut().reset();
        self.sync_cpu_to_wasm(cpu.tier1_state());

        let ret = func
            .call(&mut self.store, (self.cpu_ptr, self.jit_ctx_ptr))
            .expect("wasm tier1 block trapped");

        let exit_to_interpreter = ret == JIT_EXIT_SENTINEL_I64;
        let host_exit = *self.store.data();
        if exit_to_interpreter && host_exit.should_rollback() {
            // Restore guest RAM and CPU state snapshot.
            self.memory
                .write(&mut self.store, 0, &pre_ram)
                .expect("restore guest RAM");
            *cpu.tier1_state_mut() = pre_state.clone();

            return JitBlockExit {
                next_rip: pre_state.rip,
                exit_to_interpreter: true,
            };
        }

        self.sync_cpu_from_wasm(cpu.tier1_state_mut());

        let next_rip = if exit_to_interpreter {
            cpu.tier1_state().rip
        } else {
            ret as u64
        };

        JitBlockExit {
            next_rip,
            exit_to_interpreter,
        }
    }
}

impl<Cpu> Tier1WasmRegistry for WasmtimeBackend<Cpu> {
    fn register_tier1_block(&mut self, wasm: Vec<u8>, _exit_to_interpreter: bool) -> u32 {
        self.add_compiled_block(&wasm)
    }
}

fn define_mem_helpers(linker: &mut Linker<HostExitState>, memory: Memory) {
    fn read<const N: usize>(mem: &[u8], addr: usize) -> u64 {
        let mut v = 0u64;
        for i in 0..N {
            v |= (mem[addr + i] as u64) << (i * 8);
        }
        v
    }

    fn write<const N: usize>(mem: &mut [u8], addr: usize, value: u64) {
        for i in 0..N {
            mem[addr + i] = (value >> (i * 8)) as u8;
        }
    }

    // Reads.
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U8,
                move |caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<1>(mem.data(&caller), addr as usize) as i32
                },
            )
            .expect("define mem_read_u8");
    }
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U16,
                move |caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<2>(mem.data(&caller), addr as usize) as i32
                },
            )
            .expect("define mem_read_u16");
    }
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U32,
                move |caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<4>(mem.data(&caller), addr as usize) as i32
                },
            )
            .expect("define mem_read_u32");
    }
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U64,
                move |caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64| -> i64 {
                    read::<8>(mem.data(&caller), addr as usize) as i64
                },
            )
            .expect("define mem_read_u64");
    }

    // Writes.
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U8,
                move |mut caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<1>(mem.data_mut(&mut caller), addr as usize, value as u64);
                },
            )
            .expect("define mem_write_u8");
    }
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U16,
                move |mut caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<2>(mem.data_mut(&mut caller), addr as usize, value as u64);
                },
            )
            .expect("define mem_write_u16");
    }
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U32,
                move |mut caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<4>(mem.data_mut(&mut caller), addr as usize, value as u64);
                },
            )
            .expect("define mem_write_u32");
    }
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U64,
                move |mut caller: Caller<'_, HostExitState>, _cpu_ptr: i32, addr: i64, value: i64| {
                    write::<8>(mem.data_mut(&mut caller), addr as usize, value as u64);
                },
            )
            .expect("define mem_write_u64");
    }
}

fn define_stub_helpers(linker: &mut Linker<HostExitState>, memory: Memory) {
    // Present for ABI completeness. When called, treat these as runtime exits and roll back state.
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            |mut caller: Caller<'_, HostExitState>, _cpu_ptr: i32, _addr: i64| -> i64 {
                caller.data_mut().page_fault = true;
                JIT_EXIT_SENTINEL_I64
            },
        )
        .expect("define page_fault");

    // Minimal inline-TLB translation helper: identity map addresses that fall within the guest RAM
    // window (0..cpu_ptr) and classify anything else as MMIO.
    {
        let mem = memory;
        linker
            .func_wrap(
                IMPORT_MODULE,
                IMPORT_MMU_TRANSLATE,
                move |mut caller: Caller<'_, HostExitState>,
                      cpu_ptr: i32,
                      jit_ctx_ptr: i32,
                      vaddr: i64,
                      _access: i32|
                      -> i64 {
                    let vaddr_u = vaddr as u64;
                    let vpn = vaddr_u >> crate::PAGE_SHIFT;
                    let idx = (vpn & crate::JIT_TLB_INDEX_MASK) as u64;

                    let tlb_salt = {
                        let addr = jit_ctx_ptr as usize + (JitContext::TLB_SALT_OFFSET as usize);
                        let bytes: [u8; 8] = mem.data(&caller)[addr..addr + 8].try_into().unwrap();
                        u64::from_le_bytes(bytes)
                    };

                    // tag = (vpn ^ salt) | 1, keep tag 0 reserved for invalidation.
                    let tag = (vpn ^ tlb_salt) | 1;

                    let is_ram = vaddr_u < cpu_ptr as u64;
                    let phys_base = vaddr_u & crate::PAGE_BASE_MASK;
                    let flags = crate::TLB_FLAG_READ
                        | crate::TLB_FLAG_WRITE
                        | crate::TLB_FLAG_EXEC
                        | if is_ram { crate::TLB_FLAG_IS_RAM } else { 0 };
                    let data = phys_base | flags;

                    let entry_addr = jit_ctx_ptr as usize
                        + (JitContext::TLB_OFFSET as usize)
                        + (idx as usize) * (crate::JIT_TLB_ENTRY_SIZE as usize);
                    let mem_mut = mem.data_mut(&mut caller);
                    mem_mut[entry_addr..entry_addr + 8].copy_from_slice(&tag.to_le_bytes());
                    mem_mut[entry_addr + 8..entry_addr + 16].copy_from_slice(&data.to_le_bytes());

                    data as i64
                },
            )
            .expect("define mmu_translate");
    }

    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT_MMIO,
            |mut caller: Caller<'_, HostExitState>,
             _cpu_ptr: i32,
             _vaddr: i64,
             _size: i32,
             _is_write: i32,
             _value: i64,
             rip: i64|
              -> i64 {
                 caller.data_mut().mmio_exit = true;
                 // Return the RIP the block should resume at after the runtime has handled the
                 // MMIO access. The Tier-1 code generator returns the sentinel separately.
                 rip
              },
         )
         .expect("define jit_exit_mmio");

    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT,
            |mut caller: Caller<'_, HostExitState>, _kind: i32, rip: i64| -> i64 {
                caller.data_mut().jit_exit = true;
                // Like `jit_exit_mmio`, return the RIP to resume at while the caller uses the
                // sentinel return value to request an interpreter step.
                rip
            },
        )
        .expect("define jit_exit");
}
