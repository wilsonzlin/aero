use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemArg, MemoryType, Module, TypeSection, ValType,
};

use crate::legacy::cpu::{
    CpuState, Reg, JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK, PAGE_OFFSET_MASK,
    PAGE_SHIFT, TLB_FLAG_IS_RAM, TLB_FLAG_READ, TLB_FLAG_WRITE,
};
use crate::legacy::interp::JIT_EXIT_SENTINEL;
use crate::legacy::ir::{BinOp, CmpOp, IrBlock, IrOp, MemSize, Operand, Place, Temp};

use super::abi::{
    IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32,
    IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32,
    IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
    IMPORT_PAGE_FAULT, WASM32_MAX_PAGES,
};

/// A compiled basic block is exported as a function named `block`.
pub const EXPORT_BLOCK_FN: &str = "block";

#[derive(Debug, Clone, Copy)]
pub struct LegacyWasmOptions {
    /// Whether the imported `env.memory` is expected to be a shared memory (i.e. created with
    /// `WebAssembly.Memory({ shared: true, ... })`).
    ///
    /// Note: shared memories require a declared maximum page count.
    pub memory_shared: bool,

    /// Minimum size (in 64KiB pages) of the imported `env.memory`.
    pub memory_min_pages: u32,

    /// Maximum size (in 64KiB pages) of the imported `env.memory`.
    ///
    /// When [`Self::memory_shared`] is `true` and this is unset, the code generator defaults to
    /// 65536 pages (4GiB) so the module can accept any smaller shared memory.
    pub memory_max_pages: Option<u32>,
}

impl Default for LegacyWasmOptions {
    fn default() -> Self {
        Self {
            memory_shared: false,
            memory_min_pages: 1,
            memory_max_pages: None,
        }
    }
}

impl LegacyWasmOptions {
    fn validate_memory_import(self) {
        let effective_max_pages = if self.memory_shared {
            Some(self.memory_max_pages.unwrap_or(WASM32_MAX_PAGES))
        } else {
            self.memory_max_pages
        };

        if let Some(max) = effective_max_pages {
            assert!(
                self.memory_min_pages <= max,
                "invalid env.memory import type: min_pages ({}) > max_pages ({})",
                self.memory_min_pages,
                max
            );
        }
    }
}

#[derive(Clone, Copy)]
struct ImportedFuncs {
    mem_read_u8: u32,
    mem_read_u16: u32,
    mem_read_u32: u32,
    mem_read_u64: u32,
    mem_write_u8: u32,
    mem_write_u16: u32,
    mem_write_u32: u32,
    mem_write_u64: u32,
    mmu_translate: u32,
    _page_fault: u32,
    jit_exit_mmio: u32,
    jit_exit: u32,
    count: u32,
}

pub struct WasmCodegen;

impl WasmCodegen {
    pub fn new() -> Self {
        Self
    }

    /// Compile a single IR basic block into a standalone WASM module.
    ///
    /// ABI:
    /// - export `block(cpu_ptr: i32) -> i64`
    /// - import `env.memory`
    /// - import helpers described by the `IMPORT_*` constants
    pub fn compile_block(&self, block: &IrBlock) -> Vec<u8> {
        self.compile_block_with_options(block, LegacyWasmOptions::default())
    }

    pub fn compile_block_with_options(
        &self,
        block: &IrBlock,
        options: LegacyWasmOptions,
    ) -> Vec<u8> {
        let mut module = Module::new();

        let mut types = TypeSection::new();
        let ty_mem_read_u8 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I32]);
        let ty_mem_read_u16 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I32]);
        let ty_mem_read_u32 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I32]);
        let ty_mem_read_u64 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I64]);
        let ty_mem_write_u8 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64, ValType::I32], []);
        let ty_mem_write_u16 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64, ValType::I32], []);
        let ty_mem_write_u32 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64, ValType::I32], []);
        let ty_mem_write_u64 = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64, ValType::I64], []);
        let ty_mmu_translate = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64, ValType::I32], [ValType::I64]);
        let ty_page_fault = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I64]);
        let ty_jit_exit_mmio = types.len();
        types.ty().function(
            [
                ValType::I32,
                ValType::I64,
                ValType::I32,
                ValType::I32,
                ValType::I64,
                ValType::I64,
            ],
            [ValType::I64],
        );
        let ty_jit_exit = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I64]);
        let ty_block = types.len();
        types.ty().function([ValType::I32], [ValType::I64]);

        module.section(&types);

        options.validate_memory_import();
        let mut imports = ImportSection::new();
        let memory_max_pages: Option<u64> = if options.memory_shared {
            // Shared memories require an explicit maximum. Default to 4GiB (the maximum size of a
            // wasm32 memory) so we can link against any smaller shared memory.
            Some(u64::from(options.memory_max_pages.unwrap_or(WASM32_MAX_PAGES)))
        } else {
            options.memory_max_pages.map(u64::from)
        };
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEMORY,
            MemoryType {
                minimum: u64::from(options.memory_min_pages),
                maximum: memory_max_pages,
                memory64: false,
                shared: options.memory_shared,
                page_size_log2: None,
            },
        );

        let func_base = 0u32;
        let mut next_func = func_base;
        let imported = ImportedFuncs {
            mem_read_u8: next(&mut next_func),
            mem_read_u16: next(&mut next_func),
            mem_read_u32: next(&mut next_func),
            mem_read_u64: next(&mut next_func),
            mem_write_u8: next(&mut next_func),
            mem_write_u16: next(&mut next_func),
            mem_write_u32: next(&mut next_func),
            mem_write_u64: next(&mut next_func),
            mmu_translate: next(&mut next_func),
            _page_fault: next(&mut next_func),
            jit_exit_mmio: next(&mut next_func),
            jit_exit: next(&mut next_func),
            count: next_func - func_base,
        };
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U8,
            EntityType::Function(ty_mem_read_u8),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U16,
            EntityType::Function(ty_mem_read_u16),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U32,
            EntityType::Function(ty_mem_read_u32),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U64,
            EntityType::Function(ty_mem_read_u64),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U8,
            EntityType::Function(ty_mem_write_u8),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U16,
            EntityType::Function(ty_mem_write_u16),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U32,
            EntityType::Function(ty_mem_write_u32),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U64,
            EntityType::Function(ty_mem_write_u64),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_MMU_TRANSLATE,
            EntityType::Function(ty_mmu_translate),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            EntityType::Function(ty_page_fault),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT_MMIO,
            EntityType::Function(ty_jit_exit_mmio),
        );
        imports.import(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT,
            EntityType::Function(ty_jit_exit),
        );
        module.section(&imports);

        let mut funcs = FunctionSection::new();
        funcs.function(ty_block);
        module.section(&funcs);

        let mut exports = ExportSection::new();
        exports.export(
            EXPORT_BLOCK_FN,
            ExportKind::Func,
            imported.count, // first defined function index
        );
        module.section(&exports);

        let layout = LocalsLayout::new(block.temp_count);

        let mut code = CodeSection::new();
        let mut f = Function::new(vec![(layout.total_i64_locals(), ValType::I64)]);

        // Load GPRs into locals.
        for reg in all_regs() {
            f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
            f.instruction(&Instruction::I64Load(memarg(CpuState::reg_offset(reg), 3)));
            f.instruction(&Instruction::LocalSet(layout.reg_local(reg)));
        }
        // Load RIP.
        f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        f.instruction(&Instruction::I64Load(memarg(CpuState::RIP_OFFSET, 3)));
        f.instruction(&Instruction::LocalSet(layout.rip_local()));

        // Load guest RAM base and TLB salt (JIT metadata).
        f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        f.instruction(&Instruction::I64Load(memarg(CpuState::RAM_BASE_OFFSET, 3)));
        f.instruction(&Instruction::LocalSet(layout.ram_base_local()));

        f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        f.instruction(&Instruction::I64Load(memarg(CpuState::TLB_SALT_OFFSET, 3)));
        f.instruction(&Instruction::LocalSet(layout.tlb_salt_local()));

        // Default next_rip = current rip.
        f.instruction(&Instruction::LocalGet(layout.rip_local()));
        f.instruction(&Instruction::LocalSet(layout.next_rip_local()));

        // Structured single-exit block.
        f.instruction(&Instruction::Block(BlockType::Empty));
        let mut emitter = Emitter {
            f: &mut f,
            layout,
            imported,
            depth: 0,
        };
        emitter.emit_ops(&block.ops);
        emitter.f.instruction(&Instruction::End); // end exit block

        // Store back regs and RIP.
        for reg in all_regs() {
            emitter
                .f
                .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
            emitter
                .f
                .instruction(&Instruction::LocalGet(layout.reg_local(reg)));
            emitter
                .f
                .instruction(&Instruction::I64Store(memarg(CpuState::reg_offset(reg), 3)));
        }
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.next_rip_local()));
        emitter
            .f
            .instruction(&Instruction::I64Store(memarg(CpuState::RIP_OFFSET, 3)));

        // Return next_rip.
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.next_rip_local()));
        emitter.f.instruction(&Instruction::Return);
        emitter.f.instruction(&Instruction::End);

        code.function(&f);
        module.section(&code);

        module.finish()
    }
}

impl Default for WasmCodegen {
    fn default() -> Self {
        Self::new()
    }
}

fn next(idx: &mut u32) -> u32 {
    let cur = *idx;
    *idx += 1;
    cur
}

fn memarg(offset: u32, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}

fn all_regs() -> [Reg; Reg::COUNT] {
    [
        Reg::Rax,
        Reg::Rcx,
        Reg::Rdx,
        Reg::Rbx,
        Reg::Rsp,
        Reg::Rbp,
        Reg::Rsi,
        Reg::Rdi,
        Reg::R8,
        Reg::R9,
        Reg::R10,
        Reg::R11,
        Reg::R12,
        Reg::R13,
        Reg::R14,
        Reg::R15,
    ]
}

#[derive(Clone, Copy)]
struct LocalsLayout {
    temps: u32,
}

impl LocalsLayout {
    fn new(temps: u32) -> Self {
        Self { temps }
    }

    fn cpu_ptr_local(self) -> u32 {
        0
    }

    fn reg_local(self, reg: Reg) -> u32 {
        1 + reg as u32
    }

    fn rip_local(self) -> u32 {
        1 + Reg::COUNT as u32
    }

    fn next_rip_local(self) -> u32 {
        self.rip_local() + 1
    }

    fn ram_base_local(self) -> u32 {
        self.next_rip_local() + 1
    }

    fn tlb_salt_local(self) -> u32 {
        self.ram_base_local() + 1
    }

    fn scratch_vaddr_local(self) -> u32 {
        self.tlb_salt_local() + 1
    }

    fn scratch_vpn_local(self) -> u32 {
        self.scratch_vaddr_local() + 1
    }

    fn scratch_tlb_data_local(self) -> u32 {
        self.scratch_vpn_local() + 1
    }

    fn temp_local_base(self) -> u32 {
        self.scratch_tlb_data_local() + 1
    }

    fn temp_local(self, Temp(t): Temp) -> u32 {
        self.temp_local_base() + t
    }

    fn total_i64_locals(self) -> u32 {
        // Fixed locals:
        // - GPRs (Reg::COUNT)
        // - rip + next_rip
        // - ram_base + tlb_salt
        // - scratch locals (vaddr, vpn, tlb_data)
        Reg::COUNT as u32 + 2 + 2 + 3 + self.temps
    }
}

struct Emitter<'a> {
    f: &'a mut Function,
    layout: LocalsLayout,
    imported: ImportedFuncs,
    /// Current nesting depth *inside* the single-exit `block`.
    depth: u32,
}

impl Emitter<'_> {
    fn emit_ops(&mut self, ops: &[IrOp]) {
        for op in ops {
            self.emit_op(op);
        }
    }

    fn emit_op(&mut self, op: &IrOp) {
        match *op {
            IrOp::Set { dst, src } => {
                self.emit_operand(src);
                self.emit_set_place(dst);
            }
            IrOp::Bin { dst, op, lhs, rhs } => {
                self.emit_operand(lhs);
                self.emit_operand(rhs);
                match op {
                    BinOp::Add => {
                        self.f.instruction(&Instruction::I64Add);
                    }
                    BinOp::Sub => {
                        self.f.instruction(&Instruction::I64Sub);
                    }
                    BinOp::And => {
                        self.f.instruction(&Instruction::I64And);
                    }
                    BinOp::Or => {
                        self.f.instruction(&Instruction::I64Or);
                    }
                    BinOp::Xor => {
                        self.f.instruction(&Instruction::I64Xor);
                    }
                    BinOp::Shl => {
                        self.f.instruction(&Instruction::I64Const(63));
                        self.f.instruction(&Instruction::I64And);
                        self.f.instruction(&Instruction::I64Shl);
                    }
                    BinOp::ShrU => {
                        self.f.instruction(&Instruction::I64Const(63));
                        self.f.instruction(&Instruction::I64And);
                        self.f.instruction(&Instruction::I64ShrU);
                    }
                }
                self.emit_set_place(dst);
            }
            IrOp::Cmp { dst, op, lhs, rhs } => {
                self.emit_operand(lhs);
                self.emit_operand(rhs);
                match op {
                    CmpOp::Eq => self.f.instruction(&Instruction::I64Eq),
                    CmpOp::Ne => self.f.instruction(&Instruction::I64Ne),
                    CmpOp::LtS => self.f.instruction(&Instruction::I64LtS),
                    CmpOp::LtU => self.f.instruction(&Instruction::I64LtU),
                    CmpOp::LeS => self.f.instruction(&Instruction::I64LeS),
                    CmpOp::LeU => self.f.instruction(&Instruction::I64LeU),
                    CmpOp::GtS => self.f.instruction(&Instruction::I64GtS),
                    CmpOp::GtU => self.f.instruction(&Instruction::I64GtU),
                    CmpOp::GeS => self.f.instruction(&Instruction::I64GeS),
                    CmpOp::GeU => self.f.instruction(&Instruction::I64GeU),
                };
                self.f.instruction(&Instruction::I64ExtendI32U);
                self.emit_set_place(dst);
            }
            IrOp::Select {
                dst,
                cond,
                if_true,
                if_false,
            } => {
                self.emit_operand(if_true);
                self.emit_operand(if_false);
                self.emit_operand(cond);
                self.f.instruction(&Instruction::I64Const(0));
                self.f.instruction(&Instruction::I64Ne);
                self.f.instruction(&Instruction::Select);
                self.emit_set_place(dst);
            }
            IrOp::Load { dst, addr, size } => {
                // Save vaddr into a scratch local (used by both slow/fast paths).
                self.emit_operand(addr);
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.scratch_vaddr_local()));

                let (size_bytes, slow_read) = match size {
                    MemSize::U8 => (1u32, self.imported.mem_read_u8),
                    MemSize::U16 => (2u32, self.imported.mem_read_u16),
                    MemSize::U32 => (4u32, self.imported.mem_read_u32),
                    MemSize::U64 => (8u32, self.imported.mem_read_u64),
                };

                // Cross-page accesses are handled via the slow helper (rare but required for
                // correctness).
                let cross_limit =
                    PAGE_OFFSET_MASK.saturating_sub(u64::from(size_bytes).saturating_sub(1));
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                self.f
                    .instruction(&Instruction::I64Const(PAGE_OFFSET_MASK as i64));
                self.f.instruction(&Instruction::I64And);
                self.f
                    .instruction(&Instruction::I64Const(cross_limit as i64));
                self.f.instruction(&Instruction::I64GtU);

                self.f.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                {
                    // Slow path.
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                    self.f.instruction(&Instruction::Call(slow_read));
                    if !matches!(size, MemSize::U64) {
                        self.f.instruction(&Instruction::I64ExtendI32U);
                    }
                    self.emit_set_place(dst);
                }
                self.f.instruction(&Instruction::Else);
                {
                    // Fast path: inline JIT TLB lookup + direct RAM load.
                    self.emit_translate_and_cache(0, TLB_FLAG_READ);

                    // If the translation resolves to MMIO/ROM/unmapped, exit to runtime.
                    self.emit_mmio_exit(size_bytes, 0, None);

                    // Perform the direct linear-memory load from guest RAM.
                    self.emit_compute_ram_addr();
                    match size {
                        MemSize::U8 => self.f.instruction(&Instruction::I64Load8U(memarg(0, 0))),
                        MemSize::U16 => self.f.instruction(&Instruction::I64Load16U(memarg(0, 1))),
                        MemSize::U32 => self.f.instruction(&Instruction::I64Load32U(memarg(0, 2))),
                        MemSize::U64 => self.f.instruction(&Instruction::I64Load(memarg(0, 3))),
                    };
                    self.emit_set_place(dst);
                }
                self.f.instruction(&Instruction::End);
                self.depth -= 1;
            }
            IrOp::Store { addr, value, size } => {
                self.emit_operand(addr);
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.scratch_vaddr_local()));

                let (size_bytes, slow_write) = match size {
                    MemSize::U8 => (1u32, self.imported.mem_write_u8),
                    MemSize::U16 => (2u32, self.imported.mem_write_u16),
                    MemSize::U32 => (4u32, self.imported.mem_write_u32),
                    MemSize::U64 => (8u32, self.imported.mem_write_u64),
                };

                let cross_limit =
                    PAGE_OFFSET_MASK.saturating_sub(u64::from(size_bytes).saturating_sub(1));
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                self.f
                    .instruction(&Instruction::I64Const(PAGE_OFFSET_MASK as i64));
                self.f.instruction(&Instruction::I64And);
                self.f
                    .instruction(&Instruction::I64Const(cross_limit as i64));
                self.f.instruction(&Instruction::I64GtU);

                self.f.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                {
                    // Slow path.
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                    self.emit_operand(value);
                    if !matches!(size, MemSize::U64) {
                        self.f.instruction(&Instruction::I32WrapI64);
                    }
                    self.f.instruction(&Instruction::Call(slow_write));
                }
                self.f.instruction(&Instruction::Else);
                {
                    // Fast path: inline JIT TLB lookup + direct RAM store.
                    self.emit_translate_and_cache(1, TLB_FLAG_WRITE);

                    // Exit on MMIO/ROM/unmapped.
                    self.emit_mmio_exit(size_bytes, 1, Some(value));

                    self.emit_compute_ram_addr();
                    self.emit_operand(value);
                    match size {
                        MemSize::U8 => self.f.instruction(&Instruction::I64Store8(memarg(0, 0))),
                        MemSize::U16 => self.f.instruction(&Instruction::I64Store16(memarg(0, 1))),
                        MemSize::U32 => self.f.instruction(&Instruction::I64Store32(memarg(0, 2))),
                        MemSize::U64 => self.f.instruction(&Instruction::I64Store(memarg(0, 3))),
                    };
                }
                self.f.instruction(&Instruction::End);
                self.depth -= 1;
            }
            IrOp::Exit { next_rip } => {
                self.emit_operand(next_rip);
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
                self.f.instruction(&Instruction::Br(self.depth));
            }
            IrOp::ExitIf { cond, next_rip } => {
                self.emit_operand(cond);
                self.f.instruction(&Instruction::I64Const(0));
                self.f.instruction(&Instruction::I64Ne);

                self.f.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                self.emit_operand(next_rip);
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
                self.f.instruction(&Instruction::Br(self.depth));
                self.f.instruction(&Instruction::End);
                self.depth -= 1;
            }
            IrOp::Bailout { kind, rip } => {
                self.f.instruction(&Instruction::I32Const(kind));
                self.emit_operand(rip);
                self.f
                    .instruction(&Instruction::Call(self.imported.jit_exit));
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
                self.f.instruction(&Instruction::Br(self.depth));
            }
        }
    }

    fn emit_translate_and_cache(&mut self, access_code: i32, required_flag: u64) {
        // vpn = vaddr >> 12
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.f
            .instruction(&Instruction::I64Const(PAGE_SHIFT as i64));
        self.f.instruction(&Instruction::I64ShrU);
        self.f
            .instruction(&Instruction::LocalSet(self.layout.scratch_vpn_local()));

        // Check TLB tag match.
        self.emit_tlb_entry_addr();
        self.f.instruction(&Instruction::I64Load(memarg(0, 3))); // tag
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vpn_local()));
        self.f
            .instruction(&Instruction::LocalGet(self.layout.tlb_salt_local()));
        self.f.instruction(&Instruction::I64Xor);
        // expect_tag = (vpn ^ salt) | 1; keep 0 reserved for invalidation.
        self.f.instruction(&Instruction::I64Const(1));
        self.f.instruction(&Instruction::I64Or);
        self.f.instruction(&Instruction::I64Eq);

        self.f.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
            // Hit: load `data` from the entry.
            self.emit_tlb_entry_addr();
            self.f.instruction(&Instruction::I64Load(memarg(8, 3))); // data
            self.f
                .instruction(&Instruction::LocalSet(self.layout.scratch_tlb_data_local()));
        }
        self.f.instruction(&Instruction::Else);
        {
            // Miss: call the slow translation helper (expected to fill the entry).
            self.emit_mmu_translate(access_code);
        }
        self.f.instruction(&Instruction::End);
        self.depth -= 1;

        // Permission check: if the cached entry doesn't permit this access, go slow-path.
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
        self.f
            .instruction(&Instruction::I64Const(required_flag as i64));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I64Eqz);

        self.f.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
            self.emit_mmu_translate(access_code);
        }
        self.f.instruction(&Instruction::End);
        self.depth -= 1;
    }

    fn emit_mmu_translate(&mut self, access_code: i32) {
        self.f
            .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.f.instruction(&Instruction::I32Const(access_code));
        self.f
            .instruction(&Instruction::Call(self.imported.mmu_translate));
        self.f
            .instruction(&Instruction::LocalSet(self.layout.scratch_tlb_data_local()));
    }

    fn emit_mmio_exit(&mut self, size_bytes: u32, is_write: i32, value: Option<Operand>) {
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
        self.f
            .instruction(&Instruction::I64Const(TLB_FLAG_IS_RAM as i64));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I64Eqz);

        self.f.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
            self.f
                .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
            self.f
                .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
            self.f
                .instruction(&Instruction::I32Const(size_bytes as i32));
            self.f.instruction(&Instruction::I32Const(is_write));
            if let Some(value) = value {
                self.emit_operand(value);
            } else {
                self.f.instruction(&Instruction::I64Const(0));
            }
            self.f
                .instruction(&Instruction::LocalGet(self.layout.rip_local()));
            self.f
                .instruction(&Instruction::Call(self.imported.jit_exit_mmio));
            self.f
                .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
            self.f.instruction(&Instruction::Br(self.depth));
        }
        self.f.instruction(&Instruction::End);
        self.depth -= 1;
    }

    /// Computes the linear-memory address for the current `{vaddr, tlb_data}` pair and leaves it
    /// on the stack as an `i32` suitable for a WASM `load/store`.
    fn emit_compute_ram_addr(&mut self) {
        // paddr = (phys_base & !0xFFF) | (vaddr & 0xFFF)
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
        self.f
            .instruction(&Instruction::I64Const(PAGE_BASE_MASK as i64));
        self.f.instruction(&Instruction::I64And);

        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.f
            .instruction(&Instruction::I64Const(PAGE_OFFSET_MASK as i64));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I64Or);

        // wasm_addr = ram_base + paddr
        self.f
            .instruction(&Instruction::LocalGet(self.layout.ram_base_local()));
        self.f.instruction(&Instruction::I64Add);
        self.f.instruction(&Instruction::I32WrapI64);
    }

    fn emit_tlb_entry_addr(&mut self) {
        // base = cpu_ptr + CpuState::TLB_OFFSET + ((vpn & mask) * ENTRY_SIZE)
        self.f
            .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
        self.f.instruction(&Instruction::I64ExtendI32U);
        self.f
            .instruction(&Instruction::I64Const(CpuState::TLB_OFFSET as i64));
        self.f.instruction(&Instruction::I64Add);

        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vpn_local()));
        self.f
            .instruction(&Instruction::I64Const(JIT_TLB_INDEX_MASK as i64));
        self.f.instruction(&Instruction::I64And);
        self.f
            .instruction(&Instruction::I64Const(JIT_TLB_ENTRY_SIZE as i64));
        self.f.instruction(&Instruction::I64Mul);
        self.f.instruction(&Instruction::I64Add);
        self.f.instruction(&Instruction::I32WrapI64);
    }

    fn emit_operand(&mut self, op: Operand) {
        match op {
            Operand::Imm(v) => {
                self.f.instruction(&Instruction::I64Const(v));
            }
            Operand::Reg(r) => {
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.reg_local(r)));
            }
            Operand::Temp(t) => {
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.temp_local(t)));
            }
        }
    }

    fn emit_set_place(&mut self, place: Place) {
        match place {
            Place::Reg(r) => {
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.reg_local(r)));
            }
            Place::Temp(t) => {
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.temp_local(t)));
            }
        }
    }
}

/// The legacy bailout sentinel (same as `interp::JIT_EXIT_SENTINEL`) but as `i64`.
pub const JIT_EXIT_SENTINEL_I64: i64 = JIT_EXIT_SENTINEL as i64;
