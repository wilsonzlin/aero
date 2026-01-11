use std::marker::PhantomData;

use aero_cpu::CpuBus;
use aero_cpu::CpuState;
use aero_cpu_core::jit::runtime::{JitBackend, JitBlockExit};
use wasmtime::{Caller, Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

use super::Tier1Cpu;
use crate::abi::{CPU_AND_JIT_CTX_BYTE_SIZE, JIT_CTX_RAM_BASE_OFFSET, JIT_CTX_TLB_OFFSET, JIT_CTX_TLB_SALT_OFFSET};
use crate::wasm::tier1::EXPORT_TIER1_BLOCK_FN;
use crate::wasm::{
    IMPORT_JIT_EXIT, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE, IMPORT_MMU_TRANSLATE, IMPORT_PAGE_FAULT,
    IMPORT_JIT_EXIT_MMIO, JIT_EXIT_SENTINEL_I64,
};

/// Reference `wasmtime`-powered backend that can execute Tier-1 compiled blocks.
///
/// ## Tier-1 ABI contract (`export block(cpu_ptr: i32) -> i64`)
///
/// The compiled block receives a pointer (`cpu_ptr`) into the shared linear memory where an
/// [`aero_cpu::CpuState`] is stored. The block mutates CPU state in-place and returns an `i64`:
///
/// - `ret != JIT_EXIT_SENTINEL_I64`: `ret` is the next RIP; execution can continue in the JIT.
/// - `ret == JIT_EXIT_SENTINEL_I64`: the block requests a one-shot exit to the interpreter.
///   The concrete `next_rip` is read from `CpuState.rip` after the block has updated it.
///
/// This mirrors the existing sentinel-based contract used by the older baseline WASM codegen.
pub struct WasmtimeBackend<Cpu> {
    engine: Engine,
    store: Store<()>,
    linker: Linker<()>,
    memory: Memory,
    cpu_ptr: i32,
    blocks: Vec<TypedFunc<i32, i64>>,
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
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new(&engine);

        // A single shared memory is imported by all generated blocks.
        let memory = Memory::new(
            &mut store,
            MemoryType::new(memory_pages, None),
        )
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
        let end = (cpu_ptr as usize)
            .checked_add(CPU_AND_JIT_CTX_BYTE_SIZE as usize)
            .expect("cpu_ptr overflow");
        assert!(
            end <= byte_len,
            "cpu_ptr (0x{cpu_ptr:x}) + CPU_AND_JIT_CTX_BYTE_SIZE ({}) must fit in linear memory ({} bytes)",
            CPU_AND_JIT_CTX_BYTE_SIZE,
            byte_len
        );

        Self {
            engine,
            store,
            linker,
            memory,
            cpu_ptr,
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
            .get_typed_func::<i32, i64>(&mut self.store, EXPORT_TIER1_BLOCK_FN)
            .expect("get exported tier1 block function");
        let idx = self.blocks.len() as u32;
        self.blocks.push(func);
        idx
    }

    fn sync_cpu_to_wasm(&mut self, cpu: &CpuState) {
        let mut buf = vec![0u8; CpuState::BYTE_SIZE];
        cpu.write_to_mem(&mut buf, 0);
        self.memory
            .write(&mut self.store, self.cpu_ptr as usize, &buf)
            .expect("write CpuState into linear memory");

        // Keep the Tier-1 JIT context fields initialized even when running with the minimal
        // `aero_cpu::CpuState` prefix.
        //
        // The inline-TLB fast-path expects these fields at offsets derived from the
        // `aero_cpu_core::state::CpuState` ABI; we treat the region between the minimal CpuState
        // and the JIT context as reserved padding.
        let mem = self.memory.data_mut(&mut self.store);
        let base = self.cpu_ptr as usize;
        mem[base + JIT_CTX_RAM_BASE_OFFSET as usize..base + JIT_CTX_RAM_BASE_OFFSET as usize + 8]
            .copy_from_slice(&0u64.to_le_bytes()); // guest RAM begins at linear address 0
        mem[base + JIT_CTX_TLB_SALT_OFFSET as usize..base + JIT_CTX_TLB_SALT_OFFSET as usize + 8]
            .copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
    }

    fn sync_cpu_from_wasm(&mut self, cpu: &mut CpuState) {
        let mut buf = vec![0u8; CpuState::BYTE_SIZE];
        self.memory
            .read(&self.store, self.cpu_ptr as usize, &mut buf)
            .expect("read CpuState from linear memory");
        *cpu = CpuState::read_from_mem(&buf, 0);
    }
}

impl<Cpu> CpuBus for WasmtimeBackend<Cpu> {
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

        self.sync_cpu_to_wasm(cpu.tier1_state());

        let ret = func
            .call(&mut self.store, self.cpu_ptr)
            .expect("wasm tier1 block trapped");

        self.sync_cpu_from_wasm(cpu.tier1_state_mut());

        let exit_to_interpreter = ret == JIT_EXIT_SENTINEL_I64;
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

fn define_mem_helpers(linker: &mut Linker<()>, memory: Memory) {
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
                move |caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
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
                move |caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
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
                move |caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
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
                move |caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i64 {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i64| {
                    write::<8>(mem.data_mut(&mut caller), addr as usize, value as u64);
                },
            )
            .expect("define mem_write_u64");
    }
}

fn define_stub_helpers(linker: &mut Linker<()>, memory: Memory) {
    // Present for ABI completeness. The minimal backend does not currently model faults/MMU.
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 { JIT_EXIT_SENTINEL_I64 },
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
                move |mut caller: Caller<'_, ()>, cpu_ptr: i32, vaddr: i64, _access: i32| -> i64 {
                    let vaddr_u = vaddr as u64;
                    let vpn = vaddr_u >> crate::PAGE_SHIFT;
                    let idx = (vpn & crate::JIT_TLB_INDEX_MASK) as u64;

                    let tlb_salt = {
                        let addr = cpu_ptr as usize + JIT_CTX_TLB_SALT_OFFSET as usize;
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

                    let entry_addr = cpu_ptr as usize
                        + JIT_CTX_TLB_OFFSET as usize
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
            |_caller: Caller<'_, ()>,
             _cpu_ptr: i32,
             _vaddr: i64,
             _size: i32,
             _is_write: i32,
             _value: i64,
             _rip: i64|
             -> i64 { JIT_EXIT_SENTINEL_I64 },
        )
        .expect("define jit_exit_mmio");

    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT,
            |_caller: Caller<'_, ()>, _kind: i32, _rip: i64| -> i64 { JIT_EXIT_SENTINEL_I64 },
        )
        .expect("define jit_exit");
}
