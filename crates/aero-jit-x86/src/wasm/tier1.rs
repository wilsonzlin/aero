use aero_types::{Cond, Flag, FlagSet, Gpr, Width};
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemArg, MemoryType, Module, TypeSection, ValType,
};

use crate::abi;
use crate::abi::{MMU_ACCESS_READ, MMU_ACCESS_WRITE};
use crate::jit_ctx::JitContext;
use crate::tier1_ir::{BinOp, GuestReg, IrBlock, IrInst, IrTerminator, ValueId};

use super::abi::{
    IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32,
    IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32,
    IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
    IMPORT_PAGE_FAULT, JIT_EXIT_SENTINEL_I64,
};

/// WASM export name for Tier-1 blocks.
pub const EXPORT_TIER1_BLOCK_FN: &str = "block";

#[derive(Debug, Clone, Copy)]
pub struct Tier1WasmOptions {
    /// Enable the inline direct-mapped JIT TLB + direct guest RAM fast-path for same-page loads
    /// and stores.
    ///
    /// Note: this option is ignored unless the crate feature `tier1-inline-tlb` is enabled.
    pub inline_tlb: bool,
}

impl Default for Tier1WasmOptions {
    fn default() -> Self {
        Self { inline_tlb: false }
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
    mmu_translate: Option<u32>,
    _page_fault: u32,
    jit_exit_mmio: Option<u32>,
    _jit_exit: u32,
    count: u32,
}

pub struct Tier1WasmCodegen;

impl Tier1WasmCodegen {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn compile_block(&self, block: &IrBlock) -> Vec<u8> {
        self.compile_block_with_options(block, Tier1WasmOptions::default())
    }

    #[must_use]
    pub fn compile_block_with_options(
        &self,
        block: &IrBlock,
        options: Tier1WasmOptions,
    ) -> Vec<u8> {
        #[cfg(not(feature = "tier1-inline-tlb"))]
        let mut options = options;
        #[cfg(feature = "tier1-inline-tlb")]
        let options = options;
        #[cfg(not(feature = "tier1-inline-tlb"))]
        {
            options.inline_tlb = false;
        }
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
        let ty_mmu_translate = if options.inline_tlb {
            let ty = types.len();
            types
                .ty()
                .function(
                    [ValType::I32, ValType::I32, ValType::I64, ValType::I32],
                    [ValType::I64],
                );
            Some(ty)
        } else {
            None
        };
        let ty_page_fault = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I64]);
        let ty_jit_exit_mmio = if options.inline_tlb {
            let ty = types.len();
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
            Some(ty)
        } else {
            None
        };
        let ty_jit_exit = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I64]);
        let ty_block = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I32], [ValType::I64]);
        module.section(&types);

        let mut imports = ImportSection::new();
        imports.import(
            IMPORT_MODULE,
            IMPORT_MEMORY,
            MemoryType {
                minimum: 1,
                maximum: None,
                memory64: false,
                shared: false,
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
            mmu_translate: options.inline_tlb.then(|| next(&mut next_func)),
            _page_fault: next(&mut next_func),
            jit_exit_mmio: options.inline_tlb.then(|| next(&mut next_func)),
            _jit_exit: next(&mut next_func),
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
        if options.inline_tlb {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MMU_TRANSLATE,
                EntityType::Function(ty_mmu_translate.expect("type for mmu_translate")),
            );
        }
        imports.import(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            EntityType::Function(ty_page_fault),
        );
        if options.inline_tlb {
            imports.import(
                IMPORT_MODULE,
                IMPORT_JIT_EXIT_MMIO,
                EntityType::Function(ty_jit_exit_mmio.expect("type for jit_exit_mmio")),
            );
        }
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
        exports.export(EXPORT_TIER1_BLOCK_FN, ExportKind::Func, imported.count);
        module.section(&exports);

        let layout = LocalsLayout::new(block.value_types.len() as u32);

        let mut func = Function::new(vec![(layout.total_i64_locals(), ValType::I64)]);

        // Load architectural state into locals.
        for gpr in all_gprs() {
            func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
            func.instruction(&Instruction::I64Load(memarg(
                abi::CPU_GPR_OFF[gpr.as_u8() as usize],
                3,
            )));
            func.instruction(&Instruction::LocalSet(layout.gpr_local(gpr)));
        }
        func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        func.instruction(&Instruction::I64Load(memarg(abi::CPU_RIP_OFF, 3)));
        func.instruction(&Instruction::LocalSet(layout.rip_local()));

        func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        func.instruction(&Instruction::I64Load(memarg(abi::CPU_RFLAGS_OFF, 3)));
        func.instruction(&Instruction::LocalSet(layout.rflags_local()));

        // Default next_rip = current RIP (overwritten by terminator emission).
        func.instruction(&Instruction::LocalGet(layout.rip_local()));
        func.instruction(&Instruction::LocalSet(layout.next_rip_local()));

        if options.inline_tlb {
            // Load JIT metadata (guest RAM base and TLB salt).
            func.instruction(&Instruction::LocalGet(layout.jit_ctx_ptr_local()));
            func.instruction(&Instruction::I64Load(memarg(JitContext::RAM_BASE_OFFSET, 3)));
            func.instruction(&Instruction::LocalSet(layout.ram_base_local()));

            func.instruction(&Instruction::LocalGet(layout.jit_ctx_ptr_local()));
            func.instruction(&Instruction::I64Load(memarg(JitContext::TLB_SALT_OFFSET, 3)));
            func.instruction(&Instruction::LocalSet(layout.tlb_salt_local()));
        }

        // Structured single-exit block so we can `br` out of the block on MMIO exits.
        func.instruction(&Instruction::Block(BlockType::Empty));

        let mut emitter = Emitter {
            func: &mut func,
            imported,
            layout,
            options,
            depth: 0,
        };

        for inst in &block.insts {
            emitter.emit_inst(inst);
        }
        emitter.emit_terminator(&block.terminator);

        emitter.func.instruction(&Instruction::End); // end exit block

        // Spill guest state back to linear memory.
        for gpr in all_gprs() {
            emitter
                .func
                .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
            emitter
                .func
                .instruction(&Instruction::LocalGet(layout.gpr_local(gpr)));
            emitter.func.instruction(&Instruction::I64Store(memarg(
                abi::CPU_GPR_OFF[gpr.as_u8() as usize],
                3,
            )));
        }

        emitter
            .func
            .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        emitter
            .func
            .instruction(&Instruction::LocalGet(layout.rflags_local()));
        emitter
            .func
            .instruction(&Instruction::I64Const(abi::RFLAGS_RESERVED1 as i64));
        emitter.func.instruction(&Instruction::I64Or);
        emitter
            .func
            .instruction(&Instruction::I64Store(memarg(abi::CPU_RFLAGS_OFF, 3)));

        emitter
            .func
            .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        emitter
            .func
            .instruction(&Instruction::LocalGet(layout.next_rip_local()));
        emitter
            .func
            .instruction(&Instruction::I64Store(memarg(abi::CPU_RIP_OFF, 3)));

        emitter
            .func
            .instruction(&Instruction::LocalGet(layout.scratch_local()));
        emitter.func.instruction(&Instruction::Return);
        emitter.func.instruction(&Instruction::End);

        let mut code = CodeSection::new();
        code.function(&func);
        module.section(&code);

        module.finish()
    }
}

impl Default for Tier1WasmCodegen {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
struct LocalsLayout {
    values: u32,
}

impl LocalsLayout {
    fn new(values: u32) -> Self {
        Self { values }
    }

    fn cpu_ptr_local(self) -> u32 {
        0
    }

    fn jit_ctx_ptr_local(self) -> u32 {
        1
    }

    fn gpr_local(self, reg: Gpr) -> u32 {
        2 + reg.as_u8() as u32
    }

    fn rip_local(self) -> u32 {
        2 + 16
    }

    fn rflags_local(self) -> u32 {
        self.rip_local() + 1
    }

    fn next_rip_local(self) -> u32 {
        self.rflags_local() + 1
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

    fn scratch_local(self) -> u32 {
        self.scratch_tlb_data_local() + 1
    }

    fn value_base(self) -> u32 {
        self.scratch_local() + 1
    }

    fn value_local(self, ValueId(id): ValueId) -> u32 {
        self.value_base() + id
    }

    fn total_i64_locals(self) -> u32 {
        // gpr[16] + rip + rflags + next_rip + ram_base + tlb_salt +
        // scratch locals (vaddr, vpn, tlb_data, scratch) + values
        16 + 1 + 1 + 1 + 1 + 1 + 4 + self.values
    }
}

struct Emitter<'a> {
    func: &'a mut Function,
    imported: ImportedFuncs,
    layout: LocalsLayout,
    options: Tier1WasmOptions,
    /// Current nesting depth *inside* the single-exit `block`.
    depth: u32,
}

impl Emitter<'_> {
    fn emit_inst(&mut self, inst: &IrInst) {
        match inst {
            IrInst::Const { dst, value, width } => {
                let v = width.truncate(*value) as i64;
                self.func.instruction(&Instruction::I64Const(v));
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
            }
            IrInst::ReadReg { dst, reg } => {
                match *reg {
                    GuestReg::Rip => {
                        self.func
                            .instruction(&Instruction::LocalGet(self.layout.rip_local()));
                    }
                    GuestReg::Gpr { reg, width, high8 } => {
                        self.emit_read_gpr_part(reg, width, high8);
                    }
                    GuestReg::Flag(flag) => {
                        self.emit_read_flag(flag);
                        self.func.instruction(&Instruction::I64ExtendI32U);
                    }
                }
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
            }
            IrInst::WriteReg { reg, src } => match *reg {
                GuestReg::Rip => {
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.value_local(*src)));
                    self.func
                        .instruction(&Instruction::LocalSet(self.layout.rip_local()));
                }
                GuestReg::Gpr { reg, width, high8 } => {
                    self.emit_write_gpr_part(reg, width, high8, *src);
                }
                GuestReg::Flag(flag) => {
                    self.emit_write_flag(flag, *src);
                }
            },
            IrInst::Trunc { dst, src, width } => {
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*src)));
                self.emit_trunc(*width);
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
            }
            IrInst::Load { dst, addr, width } => {
                if !self.options.inline_tlb {
                    // Baseline mode: always go through the imported slow helpers.
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.value_local(*addr)));
                    match *width {
                        Width::W8 => {
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_read_u8));
                            self.func.instruction(&Instruction::I64ExtendI32U);
                        }
                        Width::W16 => {
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_read_u16));
                            self.func.instruction(&Instruction::I64ExtendI32U);
                        }
                        Width::W32 => {
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_read_u32));
                            self.func.instruction(&Instruction::I64ExtendI32U);
                        }
                        Width::W64 => {
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_read_u64));
                        }
                    }
                    self.emit_trunc(*width);
                    self.func
                        .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
                    return;
                }

                // Save vaddr into a scratch local (used by both slow/fast paths).
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*addr)));
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.scratch_vaddr_local()));

                let (size_bytes, slow_read) = match *width {
                    Width::W8 => (1u32, self.imported.mem_read_u8),
                    Width::W16 => (2u32, self.imported.mem_read_u16),
                    Width::W32 => (4u32, self.imported.mem_read_u32),
                    Width::W64 => (8u32, self.imported.mem_read_u64),
                };

                // Cross-page accesses use the slow helper for correctness.
                let cross_limit = (crate::PAGE_OFFSET_MASK as u64)
                    .saturating_sub(size_bytes.saturating_sub(1) as u64);
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                self.func
                    .instruction(&Instruction::I64Const(crate::PAGE_OFFSET_MASK as i64));
                self.func.instruction(&Instruction::I64And);
                self.func
                    .instruction(&Instruction::I64Const(cross_limit as i64));
                self.func.instruction(&Instruction::I64GtU);

                self.func.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                {
                    // Slow path.
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                    self.func.instruction(&Instruction::Call(slow_read));
                    if !matches!(*width, Width::W64) {
                        self.func.instruction(&Instruction::I64ExtendI32U);
                    }
                    self.emit_trunc(*width);
                    self.func
                        .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
                }
                self.func.instruction(&Instruction::Else);
                {
                    // Fast path: inline TLB probe + direct RAM load.
                    self.emit_translate_and_cache(MMU_ACCESS_READ, crate::TLB_FLAG_READ);

                    self.emit_mmio_exit(size_bytes, 0, None);

                    self.emit_compute_ram_addr();
                    match *width {
                        Width::W8 => self.func.instruction(&Instruction::I64Load8U(memarg(0, 0))),
                        Width::W16 => self
                            .func
                            .instruction(&Instruction::I64Load16U(memarg(0, 1))),
                        Width::W32 => self
                            .func
                            .instruction(&Instruction::I64Load32U(memarg(0, 2))),
                        Width::W64 => self.func.instruction(&Instruction::I64Load(memarg(0, 3))),
                    };
                    self.emit_trunc(*width);
                    self.func
                        .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
                }
                self.func.instruction(&Instruction::End);
                self.depth -= 1;
            }
            IrInst::Store { addr, src, width } => {
                if !self.options.inline_tlb {
                    // Baseline mode: always go through the imported slow helpers.
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.value_local(*addr)));
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.value_local(*src)));
                    match *width {
                        Width::W8 => {
                            self.emit_trunc(Width::W8);
                            self.func.instruction(&Instruction::I32WrapI64);
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_write_u8));
                        }
                        Width::W16 => {
                            self.emit_trunc(Width::W16);
                            self.func.instruction(&Instruction::I32WrapI64);
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_write_u16));
                        }
                        Width::W32 => {
                            self.emit_trunc(Width::W32);
                            self.func.instruction(&Instruction::I32WrapI64);
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_write_u32));
                        }
                        Width::W64 => {
                            self.func
                                .instruction(&Instruction::Call(self.imported.mem_write_u64));
                        }
                    }
                    return;
                }

                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*addr)));
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.scratch_vaddr_local()));

                let (size_bytes, slow_write) = match *width {
                    Width::W8 => (1u32, self.imported.mem_write_u8),
                    Width::W16 => (2u32, self.imported.mem_write_u16),
                    Width::W32 => (4u32, self.imported.mem_write_u32),
                    Width::W64 => (8u32, self.imported.mem_write_u64),
                };

                let cross_limit = (crate::PAGE_OFFSET_MASK as u64)
                    .saturating_sub(size_bytes.saturating_sub(1) as u64);
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                self.func
                    .instruction(&Instruction::I64Const(crate::PAGE_OFFSET_MASK as i64));
                self.func.instruction(&Instruction::I64And);
                self.func
                    .instruction(&Instruction::I64Const(cross_limit as i64));
                self.func.instruction(&Instruction::I64GtU);

                self.func.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                {
                    // Slow path.
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.value_local(*src)));
                    if !matches!(*width, Width::W64) {
                        self.emit_trunc(*width);
                        self.func.instruction(&Instruction::I32WrapI64);
                    }
                    self.func.instruction(&Instruction::Call(slow_write));
                }
                self.func.instruction(&Instruction::Else);
                {
                    // Fast path: inline TLB probe + direct RAM store.
                    self.emit_translate_and_cache(MMU_ACCESS_WRITE, crate::TLB_FLAG_WRITE);

                    self.emit_mmio_exit(size_bytes, 1, Some(*src));

                    self.emit_compute_ram_addr();
                    self.func
                        .instruction(&Instruction::LocalGet(self.layout.value_local(*src)));
                    match *width {
                        Width::W8 => self.func.instruction(&Instruction::I64Store8(memarg(0, 0))),
                        Width::W16 => self
                            .func
                            .instruction(&Instruction::I64Store16(memarg(0, 1))),
                        Width::W32 => self
                            .func
                            .instruction(&Instruction::I64Store32(memarg(0, 2))),
                        Width::W64 => self.func.instruction(&Instruction::I64Store(memarg(0, 3))),
                    };
                }
                self.func.instruction(&Instruction::End);
                self.depth -= 1;
            }
            IrInst::BinOp {
                dst,
                op,
                lhs,
                rhs,
                width,
                flags,
            } => {
                match *op {
                    BinOp::Sar => {
                        self.emit_sar(*width, *lhs, *rhs);
                    }
                    _ => {
                        self.func
                            .instruction(&Instruction::LocalGet(self.layout.value_local(*lhs)));
                        self.func
                            .instruction(&Instruction::LocalGet(self.layout.value_local(*rhs)));
                        match *op {
                            BinOp::Add => {
                                self.func.instruction(&Instruction::I64Add);
                            }
                            BinOp::Sub => {
                                self.func.instruction(&Instruction::I64Sub);
                            }
                            BinOp::And => {
                                self.func.instruction(&Instruction::I64And);
                            }
                            BinOp::Or => {
                                self.func.instruction(&Instruction::I64Or);
                            }
                            BinOp::Xor => {
                                self.func.instruction(&Instruction::I64Xor);
                            }
                            BinOp::Shl => {
                                self.emit_shift_mask(*width);
                                self.func.instruction(&Instruction::I64Shl);
                            }
                            BinOp::Shr => {
                                self.emit_shift_mask(*width);
                                self.func.instruction(&Instruction::I64ShrU);
                            }
                            BinOp::Sar => unreachable!(),
                        };
                        self.emit_trunc(*width);
                    }
                }

                self.func
                    .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));

                if !flags.is_empty() {
                    match *op {
                        BinOp::Add => self.emit_add_flags(*width, *flags, *lhs, *rhs, *dst),
                        BinOp::Sub => self.emit_sub_flags(*width, *flags, *lhs, *rhs, *dst),
                        BinOp::And | BinOp::Or | BinOp::Xor => {
                            self.emit_logic_flags(*width, *flags, *dst)
                        }
                        BinOp::Shl | BinOp::Shr | BinOp::Sar => {}
                    }
                }
            }
            IrInst::CmpFlags {
                lhs,
                rhs,
                width,
                flags,
            } => {
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*lhs)));
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*rhs)));
                self.func.instruction(&Instruction::I64Sub);
                self.emit_trunc(*width);
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.scratch_local()));
                self.emit_sub_flags_with_res(
                    *width,
                    *flags,
                    *lhs,
                    *rhs,
                    self.layout.scratch_local(),
                );
            }
            IrInst::TestFlags {
                lhs,
                rhs,
                width,
                flags,
            } => {
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*lhs)));
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*rhs)));
                self.func.instruction(&Instruction::I64And);
                self.emit_trunc(*width);
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.scratch_local()));
                self.emit_logic_flags_with_res(*width, *flags, self.layout.scratch_local());
            }
            IrInst::EvalCond { dst, cond } => {
                self.emit_eval_cond(*cond);
                self.func.instruction(&Instruction::I64ExtendI32U);
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
            }
            IrInst::Select {
                dst,
                cond,
                if_true,
                if_false,
                width,
            } => {
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*if_true)));
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*if_false)));
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(*cond)));
                self.func.instruction(&Instruction::I64Const(0));
                self.func.instruction(&Instruction::I64Ne);
                self.func.instruction(&Instruction::Select);
                self.emit_trunc(*width);
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
            }
            IrInst::CallHelper { helper, .. } => {
                // TODO: map known helpers to imports. For now, trap loudly.
                panic!("CallHelper not supported by Tier-1 WASM codegen: {helper}");
            }
        }
    }

    fn emit_terminator(&mut self, term: &IrTerminator) {
        match *term {
            IrTerminator::Jump { target } => {
                self.func.instruction(&Instruction::I64Const(target as i64));
            }
            IrTerminator::CondJump {
                cond,
                target,
                fallthrough,
            } => {
                self.func.instruction(&Instruction::I64Const(target as i64));
                self.func
                    .instruction(&Instruction::I64Const(fallthrough as i64));
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(cond)));
                self.func.instruction(&Instruction::I64Const(0));
                self.func.instruction(&Instruction::I64Ne);
                self.func.instruction(&Instruction::Select);
            }
            IrTerminator::IndirectJump { target } => {
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(target)));
            }
            IrTerminator::ExitToInterpreter { next_rip } => {
                self.func
                    .instruction(&Instruction::I64Const(next_rip as i64));
            }
        }
        self.func
            .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));

        // Encode `exit_to_interpreter` in the return value while still updating `CpuState.rip`
        // in linear memory:
        // - normal control flow returns the computed `next_rip`
        // - `ExitToInterpreter` returns `JIT_EXIT_SENTINEL_I64` and the runtime reads the real
        //   `next_rip` from `CpuState.rip`
        match *term {
            IrTerminator::ExitToInterpreter { .. } => {
                self.func
                    .instruction(&Instruction::I64Const(JIT_EXIT_SENTINEL_I64));
            }
            _ => {
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.next_rip_local()));
            }
        }
        self.func
            .instruction(&Instruction::LocalSet(self.layout.scratch_local()));
    }

    fn emit_translate_and_cache(&mut self, access_code: i32, required_flag: u64) {
        debug_assert!(self.options.inline_tlb);

        // vpn = vaddr >> 12
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.func
            .instruction(&Instruction::I64Const(crate::PAGE_SHIFT as i64));
        self.func.instruction(&Instruction::I64ShrU);
        self.func
            .instruction(&Instruction::LocalSet(self.layout.scratch_vpn_local()));

        // Check TLB tag match.
        self.emit_tlb_entry_addr();
        self.func.instruction(&Instruction::I64Load(memarg(0, 3))); // tag
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_vpn_local()));
        self.func
            .instruction(&Instruction::LocalGet(self.layout.tlb_salt_local()));
        self.func.instruction(&Instruction::I64Xor);
        // expect_tag = (vpn ^ salt) | 1; keep 0 reserved for invalidation.
        self.func.instruction(&Instruction::I64Const(1));
        self.func.instruction(&Instruction::I64Or);
        self.func.instruction(&Instruction::I64Eq);

        self.func.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
            // Hit: load `data` from the entry.
            self.emit_tlb_entry_addr();
            self.func.instruction(&Instruction::I64Load(memarg(8, 3))); // data
            self.func
                .instruction(&Instruction::LocalSet(self.layout.scratch_tlb_data_local()));
        }
        self.func.instruction(&Instruction::Else);
        {
            // Miss: call the translation helper (expected to fill the entry).
            self.emit_mmu_translate(access_code);
        }
        self.func.instruction(&Instruction::End);
        self.depth -= 1;

        // Permission check: if the cached entry doesn't permit this access, go slow-path.
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
        self.func
            .instruction(&Instruction::I64Const(required_flag as i64));
        self.func.instruction(&Instruction::I64And);
        self.func.instruction(&Instruction::I64Eqz);

        self.func.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
            self.emit_mmu_translate(access_code);
        }
        self.func.instruction(&Instruction::End);
        self.depth -= 1;
    }

    fn emit_mmu_translate(&mut self, access_code: i32) {
        self.func
            .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
        self.func
            .instruction(&Instruction::LocalGet(self.layout.jit_ctx_ptr_local()));
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.func.instruction(&Instruction::I32Const(access_code));
        self.func.instruction(&Instruction::Call(
            self.imported
                .mmu_translate
                .expect("mmu_translate import missing"),
        ));
        self.func
            .instruction(&Instruction::LocalSet(self.layout.scratch_tlb_data_local()));
    }

    fn emit_mmio_exit(&mut self, size_bytes: u32, is_write: i32, value: Option<ValueId>) {
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
        self.func
            .instruction(&Instruction::I64Const(crate::TLB_FLAG_IS_RAM as i64));
        self.func.instruction(&Instruction::I64And);
        self.func.instruction(&Instruction::I64Eqz);

        self.func.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
            self.func
                .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
            self.func
                .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
            self.func
                .instruction(&Instruction::I32Const(size_bytes as i32));
            self.func.instruction(&Instruction::I32Const(is_write));
            if let Some(value) = value {
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.value_local(value)));
            } else {
                self.func.instruction(&Instruction::I64Const(0));
            }
            self.func
                .instruction(&Instruction::LocalGet(self.layout.rip_local()));
            self.func.instruction(&Instruction::Call(
                self.imported
                    .jit_exit_mmio
                    .expect("jit_exit_mmio import missing"),
            ));
            self.func
                .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
            // MMIO exits are runtime exits; return the sentinel and let the runtime read the
            // concrete `next_rip` from `CpuState.rip`.
            self.func
                .instruction(&Instruction::I64Const(JIT_EXIT_SENTINEL_I64));
            self.func
                .instruction(&Instruction::LocalSet(self.layout.scratch_local()));
            self.func.instruction(&Instruction::Br(self.depth));
        }
        self.func.instruction(&Instruction::End);
        self.depth -= 1;
    }

    /// Computes the linear-memory address for the current `{vaddr, tlb_data}` pair and leaves it
    /// on the stack as an `i32` suitable for a WASM `load/store`.
    fn emit_compute_ram_addr(&mut self) {
        // paddr = (phys_base & !0xFFF) | (vaddr & 0xFFF)
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
        self.func
            .instruction(&Instruction::I64Const(crate::PAGE_BASE_MASK as i64));
        self.func.instruction(&Instruction::I64And);

        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.func
            .instruction(&Instruction::I64Const(crate::PAGE_OFFSET_MASK as i64));
        self.func.instruction(&Instruction::I64And);
        self.func.instruction(&Instruction::I64Or);

        // wasm_addr = ram_base + paddr
        self.func
            .instruction(&Instruction::LocalGet(self.layout.ram_base_local()));
        self.func.instruction(&Instruction::I64Add);
        self.func.instruction(&Instruction::I32WrapI64);
    }

    fn emit_tlb_entry_addr(&mut self) {
        // base = jit_ctx_ptr + JitContext::TLB_OFFSET + ((vpn & mask) * ENTRY_SIZE)
        self.func
            .instruction(&Instruction::LocalGet(self.layout.jit_ctx_ptr_local()));
        self.func.instruction(&Instruction::I64ExtendI32U);
        self.func
            .instruction(&Instruction::I64Const(JitContext::TLB_OFFSET as i64));
        self.func.instruction(&Instruction::I64Add);

        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_vpn_local()));
        self.func
            .instruction(&Instruction::I64Const(crate::JIT_TLB_INDEX_MASK as i64));
        self.func.instruction(&Instruction::I64And);
        self.func
            .instruction(&Instruction::I64Const(crate::JIT_TLB_ENTRY_SIZE as i64));
        self.func.instruction(&Instruction::I64Mul);
        self.func.instruction(&Instruction::I64Add);
        self.func.instruction(&Instruction::I32WrapI64);
    }

    fn emit_trunc(&mut self, width: Width) {
        if width == Width::W64 {
            return;
        }
        self.func
            .instruction(&Instruction::I64Const(width.mask() as i64));
        self.func.instruction(&Instruction::I64And);
    }

    fn emit_shift_mask(&mut self, width: Width) {
        let mask = (width.bits() - 1) as i64;
        self.func.instruction(&Instruction::I64Const(mask));
        self.func.instruction(&Instruction::I64And);
    }

    fn emit_read_gpr_part(&mut self, reg: Gpr, width: Width, high8: bool) {
        self.func
            .instruction(&Instruction::LocalGet(self.layout.gpr_local(reg)));
        match (width, high8) {
            (Width::W64, false) => {}
            (Width::W32, false) => {
                self.func
                    .instruction(&Instruction::I64Const(0xffff_ffffu64 as i64));
                self.func.instruction(&Instruction::I64And);
            }
            (Width::W16, false) => {
                self.func.instruction(&Instruction::I64Const(0xffff));
                self.func.instruction(&Instruction::I64And);
            }
            (Width::W8, false) => {
                self.func.instruction(&Instruction::I64Const(0xff));
                self.func.instruction(&Instruction::I64And);
            }
            (Width::W8, true) => {
                self.func.instruction(&Instruction::I64Const(8));
                self.func.instruction(&Instruction::I64ShrU);
                self.func.instruction(&Instruction::I64Const(0xff));
                self.func.instruction(&Instruction::I64And);
            }
            _ => unreachable!("invalid gpr part access: {width:?} high8={high8}"),
        }
    }

    fn emit_write_gpr_part(&mut self, reg: Gpr, width: Width, high8: bool, src: ValueId) {
        let dst_local = self.layout.gpr_local(reg);
        let src_local = self.layout.value_local(src);
        match (width, high8) {
            (Width::W64, false) => {
                self.func.instruction(&Instruction::LocalGet(src_local));
                self.func.instruction(&Instruction::LocalSet(dst_local));
            }
            (Width::W32, false) => {
                self.func.instruction(&Instruction::LocalGet(src_local));
                self.func
                    .instruction(&Instruction::I64Const(0xffff_ffffu64 as i64));
                self.func.instruction(&Instruction::I64And);
                self.func.instruction(&Instruction::LocalSet(dst_local));
            }
            (Width::W16, false) => {
                self.func.instruction(&Instruction::LocalGet(dst_local));
                self.func.instruction(&Instruction::I64Const(!0xffffi64));
                self.func.instruction(&Instruction::I64And);
                self.func.instruction(&Instruction::LocalGet(src_local));
                self.func.instruction(&Instruction::I64Const(0xffff));
                self.func.instruction(&Instruction::I64And);
                self.func.instruction(&Instruction::I64Or);
                self.func.instruction(&Instruction::LocalSet(dst_local));
            }
            (Width::W8, false) => {
                self.func.instruction(&Instruction::LocalGet(dst_local));
                self.func.instruction(&Instruction::I64Const(!0xffi64));
                self.func.instruction(&Instruction::I64And);
                self.func.instruction(&Instruction::LocalGet(src_local));
                self.func.instruction(&Instruction::I64Const(0xff));
                self.func.instruction(&Instruction::I64And);
                self.func.instruction(&Instruction::I64Or);
                self.func.instruction(&Instruction::LocalSet(dst_local));
            }
            (Width::W8, true) => {
                self.func.instruction(&Instruction::LocalGet(dst_local));
                self.func.instruction(&Instruction::I64Const(!0xff00i64));
                self.func.instruction(&Instruction::I64And);
                self.func.instruction(&Instruction::LocalGet(src_local));
                self.func.instruction(&Instruction::I64Const(0xff));
                self.func.instruction(&Instruction::I64And);
                self.func.instruction(&Instruction::I64Const(8));
                self.func.instruction(&Instruction::I64Shl);
                self.func.instruction(&Instruction::I64Or);
                self.func.instruction(&Instruction::LocalSet(dst_local));
            }
            _ => unreachable!("invalid gpr part write: {width:?} high8={high8}"),
        }
    }

    fn emit_read_flag(&mut self, flag: Flag) {
        let bit = 1u64 << flag.rflags_bit();
        self.func
            .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
        self.func.instruction(&Instruction::I64Const(bit as i64));
        self.func.instruction(&Instruction::I64And);
        self.func.instruction(&Instruction::I64Const(0));
        self.func.instruction(&Instruction::I64Ne);
    }

    fn emit_write_flag(&mut self, flag: Flag, src: ValueId) {
        let bit = 1u64 << flag.rflags_bit();
        let clear_mask = !(bit as i64);
        let bit_mask = bit as i64;
        let src_local = self.layout.value_local(src);

        self.func
            .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
        self.func.instruction(&Instruction::I64Const(bit_mask));
        self.func.instruction(&Instruction::I64Or);

        self.func
            .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
        self.func.instruction(&Instruction::I64Const(clear_mask));
        self.func.instruction(&Instruction::I64And);

        self.func.instruction(&Instruction::LocalGet(src_local));
        self.func.instruction(&Instruction::I64Const(0));
        self.func.instruction(&Instruction::I64Ne);

        self.func.instruction(&Instruction::Select);
        self.func
            .instruction(&Instruction::LocalSet(self.layout.rflags_local()));
    }

    fn emit_set_flag(&mut self, flag: Flag, emit_value: impl FnOnce(&mut Self)) {
        let bit = 1u64 << flag.rflags_bit();
        let set_mask = bit as i64;
        let clear_mask = !(bit as i64);

        // r_set
        self.func
            .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
        self.func.instruction(&Instruction::I64Const(set_mask));
        self.func.instruction(&Instruction::I64Or);

        // r_clear
        self.func
            .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
        self.func.instruction(&Instruction::I64Const(clear_mask));
        self.func.instruction(&Instruction::I64And);

        // cond_i32
        emit_value(self);

        // select + store
        self.func.instruction(&Instruction::Select);
        self.func
            .instruction(&Instruction::LocalSet(self.layout.rflags_local()));
    }

    fn emit_set_flag_const(&mut self, flag: Flag, value: bool) {
        self.emit_set_flag(flag, |this| {
            this.func
                .instruction(&Instruction::I32Const(if value { 1 } else { 0 }));
        });
    }

    fn emit_add_flags(
        &mut self,
        width: Width,
        flags: FlagSet,
        lhs: ValueId,
        rhs: ValueId,
        res: ValueId,
    ) {
        self.emit_addsub_flags(width, flags, lhs, rhs, self.layout.value_local(res), true);
    }

    fn emit_sub_flags(
        &mut self,
        width: Width,
        flags: FlagSet,
        lhs: ValueId,
        rhs: ValueId,
        res: ValueId,
    ) {
        self.emit_addsub_flags(width, flags, lhs, rhs, self.layout.value_local(res), false);
    }

    fn emit_sub_flags_with_res(
        &mut self,
        width: Width,
        flags: FlagSet,
        lhs: ValueId,
        rhs: ValueId,
        res_local: u32,
    ) {
        self.emit_addsub_flags(width, flags, lhs, rhs, res_local, false);
    }

    fn emit_logic_flags(&mut self, width: Width, flags: FlagSet, res: ValueId) {
        self.emit_logic_flags_with_res(width, flags, self.layout.value_local(res));
    }

    fn emit_logic_flags_with_res(&mut self, width: Width, flags: FlagSet, res_local: u32) {
        let sign_bit = 1u64 << (width.bits() - 1);

        if flags.contains(FlagSet::CF) {
            self.emit_set_flag_const(Flag::Cf, false);
        }
        if flags.contains(FlagSet::OF) {
            self.emit_set_flag_const(Flag::Of, false);
        }
        if flags.contains(FlagSet::AF) {
            // Tier-1 interpreter forces AF=false for logic ops.
            self.emit_set_flag_const(Flag::Af, false);
        }

        if flags.contains(FlagSet::ZF) {
            self.emit_set_flag(Flag::Zf, |this| {
                this.func.instruction(&Instruction::LocalGet(res_local));
                this.func.instruction(&Instruction::I64Eqz);
            });
        }
        if flags.contains(FlagSet::SF) {
            self.emit_set_flag(Flag::Sf, |this| {
                this.func.instruction(&Instruction::LocalGet(res_local));
                this.func
                    .instruction(&Instruction::I64Const(sign_bit as i64));
                this.func.instruction(&Instruction::I64And);
                this.func.instruction(&Instruction::I64Const(0));
                this.func.instruction(&Instruction::I64Ne);
            });
        }
        if flags.contains(FlagSet::PF) {
            self.emit_set_flag(Flag::Pf, |this| {
                this.emit_parity_even_i32(res_local);
            });
        }
    }

    fn emit_addsub_flags(
        &mut self,
        width: Width,
        flags: FlagSet,
        lhs: ValueId,
        rhs: ValueId,
        res_local: u32,
        is_add: bool,
    ) {
        let sign_bit = 1u64 << (width.bits() - 1);

        let lhs_local = self.layout.value_local(lhs);
        let rhs_local = self.layout.value_local(rhs);

        if flags.contains(FlagSet::CF) {
            self.emit_set_flag(Flag::Cf, |this| {
                if is_add {
                    this.func.instruction(&Instruction::LocalGet(res_local));
                    this.func.instruction(&Instruction::LocalGet(lhs_local));
                    this.func.instruction(&Instruction::I64LtU);
                } else {
                    this.func.instruction(&Instruction::LocalGet(lhs_local));
                    this.func.instruction(&Instruction::LocalGet(rhs_local));
                    this.func.instruction(&Instruction::I64LtU);
                }
            });
        }

        if flags.contains(FlagSet::ZF) {
            self.emit_set_flag(Flag::Zf, |this| {
                this.func.instruction(&Instruction::LocalGet(res_local));
                this.func.instruction(&Instruction::I64Eqz);
            });
        }

        if flags.contains(FlagSet::SF) {
            self.emit_set_flag(Flag::Sf, |this| {
                this.func.instruction(&Instruction::LocalGet(res_local));
                this.func
                    .instruction(&Instruction::I64Const(sign_bit as i64));
                this.func.instruction(&Instruction::I64And);
                this.func.instruction(&Instruction::I64Const(0));
                this.func.instruction(&Instruction::I64Ne);
            });
        }

        if flags.contains(FlagSet::PF) {
            self.emit_set_flag(Flag::Pf, |this| {
                this.emit_parity_even_i32(res_local);
            });
        }

        if flags.contains(FlagSet::AF) {
            self.emit_set_flag(Flag::Af, |this| {
                this.func.instruction(&Instruction::LocalGet(lhs_local));
                this.func.instruction(&Instruction::LocalGet(rhs_local));
                this.func.instruction(&Instruction::I64Xor);
                this.func.instruction(&Instruction::LocalGet(res_local));
                this.func.instruction(&Instruction::I64Xor);
                this.func.instruction(&Instruction::I64Const(0x10));
                this.func.instruction(&Instruction::I64And);
                this.func.instruction(&Instruction::I64Const(0));
                this.func.instruction(&Instruction::I64Ne);
            });
        }

        if flags.contains(FlagSet::OF) {
            self.emit_set_flag(Flag::Of, |this| {
                if is_add {
                    this.func.instruction(&Instruction::LocalGet(lhs_local));
                    this.func.instruction(&Instruction::LocalGet(res_local));
                    this.func.instruction(&Instruction::I64Xor);
                    this.func.instruction(&Instruction::LocalGet(rhs_local));
                    this.func.instruction(&Instruction::LocalGet(res_local));
                    this.func.instruction(&Instruction::I64Xor);
                    this.func.instruction(&Instruction::I64And);
                } else {
                    this.func.instruction(&Instruction::LocalGet(lhs_local));
                    this.func.instruction(&Instruction::LocalGet(rhs_local));
                    this.func.instruction(&Instruction::I64Xor);
                    this.func.instruction(&Instruction::LocalGet(lhs_local));
                    this.func.instruction(&Instruction::LocalGet(res_local));
                    this.func.instruction(&Instruction::I64Xor);
                    this.func.instruction(&Instruction::I64And);
                }
                this.func
                    .instruction(&Instruction::I64Const(sign_bit as i64));
                this.func.instruction(&Instruction::I64And);
                this.func.instruction(&Instruction::I64Const(0));
                this.func.instruction(&Instruction::I64Ne);
            });
        }
    }

    fn emit_parity_even_i32(&mut self, res_local: u32) {
        self.func.instruction(&Instruction::LocalGet(res_local));
        self.func.instruction(&Instruction::I64Const(0xff));
        self.func.instruction(&Instruction::I64And);
        self.func.instruction(&Instruction::I32WrapI64);
        self.func.instruction(&Instruction::I32Popcnt);
        self.func.instruction(&Instruction::I32Const(1));
        self.func.instruction(&Instruction::I32And);
        self.func.instruction(&Instruction::I32Eqz);
    }

    fn emit_eval_cond(&mut self, cond: Cond) {
        let read = |this: &mut Self, flag: Flag| {
            this.emit_read_flag(flag);
        };
        match cond {
            Cond::O => read(self, Flag::Of),
            Cond::No => {
                read(self, Flag::Of);
                self.func.instruction(&Instruction::I32Eqz);
            }
            Cond::B => read(self, Flag::Cf),
            Cond::Ae => {
                read(self, Flag::Cf);
                self.func.instruction(&Instruction::I32Eqz);
            }
            Cond::E => read(self, Flag::Zf),
            Cond::Ne => {
                read(self, Flag::Zf);
                self.func.instruction(&Instruction::I32Eqz);
            }
            Cond::Be => {
                read(self, Flag::Cf);
                read(self, Flag::Zf);
                self.func.instruction(&Instruction::I32Or);
            }
            Cond::A => {
                read(self, Flag::Cf);
                self.func.instruction(&Instruction::I32Eqz);
                read(self, Flag::Zf);
                self.func.instruction(&Instruction::I32Eqz);
                self.func.instruction(&Instruction::I32And);
            }
            Cond::S => read(self, Flag::Sf),
            Cond::Ns => {
                read(self, Flag::Sf);
                self.func.instruction(&Instruction::I32Eqz);
            }
            Cond::P => read(self, Flag::Pf),
            Cond::Np => {
                read(self, Flag::Pf);
                self.func.instruction(&Instruction::I32Eqz);
            }
            Cond::L => {
                read(self, Flag::Sf);
                read(self, Flag::Of);
                self.func.instruction(&Instruction::I32Xor);
            }
            Cond::Ge => {
                read(self, Flag::Sf);
                read(self, Flag::Of);
                self.func.instruction(&Instruction::I32Eq);
            }
            Cond::Le => {
                read(self, Flag::Zf);
                read(self, Flag::Sf);
                read(self, Flag::Of);
                self.func.instruction(&Instruction::I32Xor);
                self.func.instruction(&Instruction::I32Or);
            }
            Cond::G => {
                read(self, Flag::Zf);
                self.func.instruction(&Instruction::I32Eqz);
                read(self, Flag::Sf);
                read(self, Flag::Of);
                self.func.instruction(&Instruction::I32Eq);
                self.func.instruction(&Instruction::I32And);
            }
        }
    }

    fn emit_sar(&mut self, width: Width, lhs: ValueId, rhs: ValueId) {
        let mask = width.mask();
        let sign_bit = 1u64 << (width.bits() - 1);

        // scratch = lhs truncated
        self.func
            .instruction(&Instruction::LocalGet(self.layout.value_local(lhs)));
        self.emit_trunc(width);
        self.func
            .instruction(&Instruction::LocalSet(self.layout.scratch_local()));

        // if_sign = scratch | !mask
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_local()));
        self.func
            .instruction(&Instruction::I64Const(!(mask as i64)));
        self.func.instruction(&Instruction::I64Or);

        // else = scratch
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_local()));

        // sign_cond = (scratch & sign_bit) != 0
        self.func
            .instruction(&Instruction::LocalGet(self.layout.scratch_local()));
        self.func
            .instruction(&Instruction::I64Const(sign_bit as i64));
        self.func.instruction(&Instruction::I64And);
        self.func.instruction(&Instruction::I64Const(0));
        self.func.instruction(&Instruction::I64Ne);

        // select sign-extended value
        self.func.instruction(&Instruction::Select);

        // shift amount
        self.func
            .instruction(&Instruction::LocalGet(self.layout.value_local(rhs)));
        self.emit_shift_mask(width);
        self.func.instruction(&Instruction::I64ShrS);
        self.emit_trunc(width);
    }
}

impl Default for ImportedFuncs {
    fn default() -> Self {
        Self {
            mem_read_u8: 0,
            mem_read_u16: 0,
            mem_read_u32: 0,
            mem_read_u64: 0,
            mem_write_u8: 0,
            mem_write_u16: 0,
            mem_write_u32: 0,
            mem_write_u64: 0,
            mmu_translate: None,
            _page_fault: 0,
            jit_exit_mmio: None,
            _jit_exit: 0,
            count: 0,
        }
    }
}

fn all_gprs() -> [Gpr; 16] {
    [
        Gpr::Rax,
        Gpr::Rcx,
        Gpr::Rdx,
        Gpr::Rbx,
        Gpr::Rsp,
        Gpr::Rbp,
        Gpr::Rsi,
        Gpr::Rdi,
        Gpr::R8,
        Gpr::R9,
        Gpr::R10,
        Gpr::R11,
        Gpr::R12,
        Gpr::R13,
        Gpr::R14,
        Gpr::R15,
    ]
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
