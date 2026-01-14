use aero_types::{Cond, Flag, FlagSet, Gpr, Width};
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemArg, MemoryType, Module, TypeSection, ValType,
};

use super::ir::{BinOp, GuestReg, IrBlock, IrInst, IrTerminator, ValueId};
use crate::abi;
use crate::abi::{MMU_ACCESS_READ, MMU_ACCESS_WRITE};
use crate::jit_ctx::{self, JitContext};
use crate::wasm::inline_tlb_codegen::{self, InlineTlbLocals};

use crate::wasm::abi::{
    IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32,
    IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32,
    IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
    JIT_EXIT_SENTINEL_I64, WASM32_MAX_PAGES,
};

/// WASM export name for Tier-1 blocks.
pub const EXPORT_BLOCK_FN: &str = crate::wasm::abi::EXPORT_BLOCK_FN;

/// Backwards-compatible alias for [`EXPORT_BLOCK_FN`].
pub const EXPORT_TIER1_BLOCK_FN: &str = EXPORT_BLOCK_FN;

#[derive(Debug, Clone, Copy)]
pub struct Tier1WasmOptions {
    /// Enable the inline direct-mapped JIT TLB + direct guest RAM fast-path for same-page loads.
    ///
    /// Note: this option is ignored unless the crate feature `tier1-inline-tlb` is enabled.
    pub inline_tlb: bool,

    /// When [`Self::inline_tlb`] is enabled, allow stores to use the same inline-TLB fast-path as
    /// loads.
    ///
    /// When disabled, Tier-1 stores always go through the imported slow helpers (`env.mem_write_*`)
    /// so the host runtime can observe writes (MMIO classification, self-modifying code
    /// invalidation via `jit.on_guest_write(..)`, etc).
    pub inline_tlb_stores: bool,

    /// When [`Self::inline_tlb`] is enabled, control how same-page non-RAM translations are
    /// handled.
    ///
    /// - When `true` (default), Tier-1 treats these accesses as MMIO and calls the imported
    ///   `env.jit_exit_mmio` helper, causing the backend to roll back guest state and resume via
    ///   the interpreter.
    /// - When `false`, Tier-1 falls back to the imported slow mem helpers (`env.mem_read_*` /
    ///   `env.mem_write_*`) and continues executing the block.
    pub inline_tlb_mmio_exit: bool,

    /// When [`Self::inline_tlb`] is enabled, allow cross-page (4KiB boundary-crossing) loads/stores
    /// to use an inline-TLB RAM fast-path instead of immediately falling back to the imported slow
    /// helpers (`env.mem_read_*` / `env.mem_write_*`).
    ///
    /// When enabled and an access crosses a page boundary, the code generator emits a split access
    /// that translates both pages and performs direct guest-RAM loads/stores for each page.
    ///
    /// Note: this option is ignored unless the crate feature `tier1-inline-tlb` is enabled.
    pub inline_tlb_cross_page_fastpath: bool,

    /// Whether the imported `env.memory` is expected to be a shared memory (i.e. created with
    /// `WebAssembly.Memory({ shared: true, ... })`).
    ///
    /// Note: shared memories require a declared maximum page count.
    pub memory_shared: bool,

    /// Minimum size (in 64KiB pages) of the imported `env.memory`.
    pub memory_min_pages: u32,

    /// Maximum size (in 64KiB pages) of the imported `env.memory`.
    ///
    /// When [`Tier1WasmOptions::memory_shared`] is `true` and this is unset, the code generator
    /// defaults to 65536 pages (4GiB) so the module can accept any smaller shared memory.
    pub memory_max_pages: Option<u32>,
}

impl Default for Tier1WasmOptions {
    fn default() -> Self {
        Self {
            inline_tlb: false,
            // Preserve historical behaviour: when callers enable `inline_tlb`, stores take the
            // fast-path by default unless explicitly disabled.
            inline_tlb_stores: true,
            // Preserve existing behaviour by default: MMIO/non-RAM access forces a runtime exit.
            inline_tlb_mmio_exit: true,
            // Preserve historical behaviour: cross-page accesses always used the slow helpers.
            inline_tlb_cross_page_fastpath: false,
            // Preserve existing behaviour by default: import an unshared memory with min=1 and no
            // maximum.
            memory_shared: false,
            memory_min_pages: 1,
            memory_max_pages: None,
        }
    }
}

impl Tier1WasmOptions {
    fn validate_memory_import(self) {
        let effective_max_pages = if self.memory_shared {
            Some(self.memory_max_pages.unwrap_or(WASM32_MAX_PAGES))
        } else {
            self.memory_max_pages
        };

        assert!(
            self.memory_min_pages <= WASM32_MAX_PAGES,
            "invalid env.memory import type: min_pages ({}) exceeds wasm32 max pages ({})",
            self.memory_min_pages,
            WASM32_MAX_PAGES
        );
        if let Some(max) = effective_max_pages {
            assert!(
                max <= WASM32_MAX_PAGES,
                "invalid env.memory import type: max_pages ({}) exceeds wasm32 max pages ({})",
                max,
                WASM32_MAX_PAGES
            );
            assert!(
                self.memory_min_pages <= max,
                "invalid env.memory import type: min_pages ({}) > max_pages ({})",
                self.memory_min_pages,
                max
            );
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ImportedFuncs {
    mem_read_u8: Option<u32>,
    mem_read_u16: Option<u32>,
    mem_read_u32: Option<u32>,
    mem_read_u64: Option<u32>,
    mem_write_u8: Option<u32>,
    mem_write_u16: Option<u32>,
    mem_write_u32: Option<u32>,
    mem_write_u64: Option<u32>,
    mmu_translate: Option<u32>,
    jit_exit_mmio: Option<u32>,
    jit_exit: Option<u32>,
    count: u32,
}

pub struct Tier1WasmCodegen;

#[derive(Debug, Clone, Copy, Default)]
struct BlockStateUsage {
    /// Whether a GPR's *initial* value must be loaded from `CpuState` at block entry.
    ///
    /// This is required when the block reads the GPR, or when the block performs a partial write
    /// (8/16/high8) that needs the previous 64-bit value.
    gpr_used: [bool; 16],
    /// Whether a GPR may be written by the block and therefore must be spilled back to `CpuState`
    /// at block exit.
    gpr_written: [bool; 16],
    /// Whether the block's RIP local must be initialized from `CpuState.rip` at block entry.
    ///
    /// Tier-1 blocks always write back `next_rip` at exit, but loading the *current* RIP is only
    /// needed when the IR reads it directly or when Tier-1 needs to pass it to runtime exit
    /// helpers (MMIO exits / `CallHelper` bailouts).
    rip_used: bool,
    /// Whether the block reads and/or writes RFLAGS.
    uses_rflags: bool,
    /// Whether the block writes RFLAGS and therefore must spill `CpuState.rflags` at exit.
    rflags_written: bool,
}

fn analyze_state_usage(block: &IrBlock, options: Tier1WasmOptions) -> BlockStateUsage {
    let mut usage = BlockStateUsage::default();
    let mut initialized = [false; 16];
    let mut first_write_idx: [Option<usize>; 16] = [None; 16];
    let mut rip_initialized = false;

    let mut earliest_may_exit: Option<usize> = None;

    for (i, inst) in block.insts.iter().enumerate() {
        // Conservative: helper calls always bail out.
        if matches!(inst, IrInst::CallHelper { .. }) {
            earliest_may_exit = earliest_may_exit.or(Some(i));
        } else if options.inline_tlb && options.inline_tlb_mmio_exit {
            // Inline-TLB fast paths can exit early on MMIO when configured to do so.
            let is_mmio_exit_point = match inst {
                IrInst::Load { .. } => true,
                IrInst::Store { .. } => options.inline_tlb_stores,
                _ => false,
            };
            if is_mmio_exit_point {
                earliest_may_exit = earliest_may_exit.or(Some(i));
            }
        }

        match inst {
            IrInst::ReadReg { reg, .. } => match *reg {
                GuestReg::Gpr { reg, .. } => {
                    let idx = reg.as_u8() as usize;
                    if !initialized[idx] {
                        usage.gpr_used[idx] = true;
                        initialized[idx] = true;
                    }
                }
                GuestReg::Flag(_) => usage.uses_rflags = true,
                GuestReg::Rip => {
                    if !rip_initialized {
                        usage.rip_used = true;
                        rip_initialized = true;
                    }
                }
            },
            IrInst::WriteReg { reg, .. } => match *reg {
                GuestReg::Gpr { reg, width, high8 } => {
                    let idx = reg.as_u8() as usize;
                    usage.gpr_written[idx] = true;
                    first_write_idx[idx].get_or_insert(i);

                    let is_partial = matches!(width, Width::W8 | Width::W16) || high8;
                    if is_partial && !initialized[idx] {
                        // Partial writes need the previous 64-bit value.
                        usage.gpr_used[idx] = true;
                        initialized[idx] = true;
                    }

                    // Any write (full or partial) initializes the local for subsequent accesses.
                    initialized[idx] = true;
                }
                GuestReg::Flag(_) => {
                    usage.uses_rflags = true;
                    usage.rflags_written = true;
                }
                GuestReg::Rip => rip_initialized = true,
            },
            IrInst::BinOp { flags, .. }
            | IrInst::CmpFlags { flags, .. }
            | IrInst::TestFlags { flags, .. } => {
                if !flags.is_empty() {
                    usage.uses_rflags = true;
                    usage.rflags_written = true;
                }
            }
            IrInst::EvalCond { cond, .. } => {
                if !cond.uses_flags().is_empty() {
                    usage.uses_rflags = true;
                }
            }
            IrInst::CallHelper { .. } => {
                // Helper-call bailouts pass the current RIP to the host.
                if !rip_initialized {
                    usage.rip_used = true;
                }
                // Tier-1 treats helper calls as a runtime exit; the remainder of the IR block is
                // unreachable (both in the IR interpreter and in Tier-1 WASM codegen).
                break;
            }
            IrInst::Load { .. } => {
                // Inline-TLB loads may take an MMIO exit and must pass the current RIP.
                if options.inline_tlb && options.inline_tlb_mmio_exit && !rip_initialized {
                    usage.rip_used = true;
                    rip_initialized = true;
                }
            }
            IrInst::Store { .. } => {
                // Inline-TLB stores may take an MMIO exit (when store fast-path is enabled) and
                // must pass the current RIP.
                if options.inline_tlb
                    && options.inline_tlb_mmio_exit
                    && options.inline_tlb_stores
                    && !rip_initialized
                {
                    usage.rip_used = true;
                    rip_initialized = true;
                }
            }
            _ => {}
        }
    }

    // If the block can exit early (MMIO fast path or CallHelper bailout) before a GPR's first
    // write, we still need to load that GPR if we're going to spill it at function exit; otherwise
    // the epilogue store would clobber the register with the local's default value (0).
    if let Some(exit_idx) = earliest_may_exit {
        for gpr in all_gprs() {
            let idx = gpr.as_u8() as usize;
            if !usage.gpr_written[idx] || usage.gpr_used[idx] {
                continue;
            }
            if let Some(first_write) = first_write_idx[idx] {
                if exit_idx < first_write {
                    usage.gpr_used[idx] = true;
                }
            }
        }
    }

    usage
}

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
        let mut options = options;
        #[cfg(not(feature = "tier1-inline-tlb"))]
        {
            options.inline_tlb = false;
        }

        let mut needs_mem_read_u8 = false;
        let mut needs_mem_read_u16 = false;
        let mut needs_mem_read_u32 = false;
        let mut needs_mem_read_u64 = false;
        let mut needs_mem_write_u8 = false;
        let mut needs_mem_write_u16 = false;
        let mut needs_mem_write_u32 = false;
        let mut needs_mem_write_u64 = false;
        let mut needs_jit_exit = false;
        let mut uses_inline_tlb = false;
        for inst in &block.insts {
            match inst {
                IrInst::Load { width, .. } => {
                    uses_inline_tlb = true;
                    match *width {
                        Width::W8 => needs_mem_read_u8 = true,
                        Width::W16 => needs_mem_read_u16 = true,
                        Width::W32 => needs_mem_read_u32 = true,
                        Width::W64 => needs_mem_read_u64 = true,
                    }
                }
                IrInst::Store { width, .. } => {
                    if options.inline_tlb_stores {
                        uses_inline_tlb = true;
                    }
                    match *width {
                        Width::W8 => needs_mem_write_u8 = true,
                        Width::W16 => needs_mem_write_u16 = true,
                        Width::W32 => needs_mem_write_u32 = true,
                        Width::W64 => needs_mem_write_u64 = true,
                    }
                }
                IrInst::CallHelper { .. } => needs_jit_exit = true,
                _ => {}
            }
        }

        // Enabling the inline-TLB fast-path only matters if the block contains a memory operation
        // eligible for it.
        if options.inline_tlb && !uses_inline_tlb {
            options.inline_tlb = false;
        }

        let mut module = Module::new();

        let mut types = TypeSection::new();
        // Keep the type section minimal: only emit helper import types that are actually used by
        // this block.
        let ty_mem_read_u8 = needs_mem_read_u8.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64], [ValType::I32]);
            ty
        });
        let ty_mem_read_u16 = needs_mem_read_u16.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64], [ValType::I32]);
            ty
        });
        let ty_mem_read_u32 = needs_mem_read_u32.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64], [ValType::I32]);
            ty
        });
        let ty_mem_read_u64 = needs_mem_read_u64.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64], [ValType::I64]);
            ty
        });
        let ty_mem_write_u8 = needs_mem_write_u8.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64, ValType::I32], []);
            ty
        });
        let ty_mem_write_u16 = needs_mem_write_u16.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64, ValType::I32], []);
            ty
        });
        let ty_mem_write_u32 = needs_mem_write_u32.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64, ValType::I32], []);
            ty
        });
        let ty_mem_write_u64 = needs_mem_write_u64.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64, ValType::I64], []);
            ty
        });
        let ty_mmu_translate = if options.inline_tlb {
            let ty = types.len();
            types.ty().function(
                [ValType::I32, ValType::I32, ValType::I64, ValType::I32],
                [ValType::I64],
            );
            Some(ty)
        } else {
            None
        };
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
        let ty_jit_exit = needs_jit_exit.then(|| {
            let ty = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64], [ValType::I64]);
            ty
        });
        let ty_block = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I32], [ValType::I64]);
        module.section(&types);

        options.validate_memory_import();
        let mut imports = ImportSection::new();
        let memory_max_pages: Option<u64> = if options.memory_shared {
            // Shared memories require an explicit maximum. Default to 4GiB (the maximum size of a
            // wasm32 memory) so we can link against any smaller shared memory.
            Some(u64::from(
                options.memory_max_pages.unwrap_or(WASM32_MAX_PAGES),
            ))
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
            mem_read_u8: needs_mem_read_u8.then(|| next(&mut next_func)),
            mem_read_u16: needs_mem_read_u16.then(|| next(&mut next_func)),
            mem_read_u32: needs_mem_read_u32.then(|| next(&mut next_func)),
            mem_read_u64: needs_mem_read_u64.then(|| next(&mut next_func)),
            mem_write_u8: needs_mem_write_u8.then(|| next(&mut next_func)),
            mem_write_u16: needs_mem_write_u16.then(|| next(&mut next_func)),
            mem_write_u32: needs_mem_write_u32.then(|| next(&mut next_func)),
            mem_write_u64: needs_mem_write_u64.then(|| next(&mut next_func)),
            mmu_translate: options.inline_tlb.then(|| next(&mut next_func)),
            jit_exit_mmio: options.inline_tlb.then(|| next(&mut next_func)),
            jit_exit: needs_jit_exit.then(|| next(&mut next_func)),
            count: next_func - func_base,
        };

        if needs_mem_read_u8 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U8,
                EntityType::Function(ty_mem_read_u8.expect("type for mem_read_u8")),
            );
        }
        if needs_mem_read_u16 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U16,
                EntityType::Function(ty_mem_read_u16.expect("type for mem_read_u16")),
            );
        }
        if needs_mem_read_u32 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U32,
                EntityType::Function(ty_mem_read_u32.expect("type for mem_read_u32")),
            );
        }
        if needs_mem_read_u64 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U64,
                EntityType::Function(ty_mem_read_u64.expect("type for mem_read_u64")),
            );
        }
        if needs_mem_write_u8 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U8,
                EntityType::Function(ty_mem_write_u8.expect("type for mem_write_u8")),
            );
        }
        if needs_mem_write_u16 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U16,
                EntityType::Function(ty_mem_write_u16.expect("type for mem_write_u16")),
            );
        }
        if needs_mem_write_u32 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U32,
                EntityType::Function(ty_mem_write_u32.expect("type for mem_write_u32")),
            );
        }
        if needs_mem_write_u64 {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U64,
                EntityType::Function(ty_mem_write_u64.expect("type for mem_write_u64")),
            );
        }
        if options.inline_tlb {
            imports.import(
                IMPORT_MODULE,
                IMPORT_MMU_TRANSLATE,
                EntityType::Function(ty_mmu_translate.expect("type for mmu_translate")),
            );
        }
        if options.inline_tlb {
            imports.import(
                IMPORT_MODULE,
                IMPORT_JIT_EXIT_MMIO,
                EntityType::Function(ty_jit_exit_mmio.expect("type for jit_exit_mmio")),
            );
        }
        if needs_jit_exit {
            imports.import(
                IMPORT_MODULE,
                IMPORT_JIT_EXIT,
                EntityType::Function(ty_jit_exit.expect("type for jit_exit")),
            );
        }
        module.section(&imports);

        let mut funcs = FunctionSection::new();
        funcs.function(ty_block);
        module.section(&funcs);

        let mut exports = ExportSection::new();
        exports.export(EXPORT_BLOCK_FN, ExportKind::Func, imported.count);
        module.section(&exports);

        let layout = LocalsLayout::new(block.value_types.len() as u32);
        let state_usage = analyze_state_usage(block, options);

        let mut func = Function::new(vec![(layout.total_i64_locals(), ValType::I64)]);

        // Load architectural state into locals.
        for gpr in all_gprs() {
            if state_usage.gpr_used[gpr.as_u8() as usize] {
                func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
                func.instruction(&Instruction::I64Load(memarg(
                    abi::CPU_GPR_OFF[gpr.as_u8() as usize],
                    3,
                )));
                func.instruction(&Instruction::LocalSet(layout.gpr_local(gpr)));
            }
        }
        if state_usage.rip_used {
            func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
            func.instruction(&Instruction::I64Load(memarg(abi::CPU_RIP_OFF, 3)));
            func.instruction(&Instruction::LocalSet(layout.rip_local()));
        }

        if state_usage.uses_rflags {
            func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
            func.instruction(&Instruction::I64Load(memarg(abi::CPU_RFLAGS_OFF, 3)));
            func.instruction(&Instruction::LocalSet(layout.rflags_local()));
        }

        if options.inline_tlb {
            let has_store_mem = options.inline_tlb_stores
                && block
                    .insts
                    .iter()
                    .any(|inst| matches!(inst, IrInst::Store { .. }));
            if has_store_mem {
                // Cache the code-version table pointer and length in locals so the RAM write
                // fast-path can bump code page versions without repeated loads from `cpu_ptr`.
                func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
                func.instruction(&Instruction::I32Load(memarg(
                    jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET,
                    2,
                )));
                func.instruction(&Instruction::I64ExtendI32U);
                func.instruction(&Instruction::LocalSet(
                    layout.code_version_table_ptr_local(),
                ));

                func.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
                func.instruction(&Instruction::I32Load(memarg(
                    jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET,
                    2,
                )));
                func.instruction(&Instruction::I64ExtendI32U);
                func.instruction(&Instruction::LocalSet(
                    layout.code_version_table_len_local(),
                ));
            }

            // Load JIT metadata (guest RAM base and TLB salt).
            func.instruction(&Instruction::LocalGet(layout.jit_ctx_ptr_local()));
            func.instruction(&Instruction::I64Load(memarg(
                JitContext::RAM_BASE_OFFSET,
                3,
            )));
            func.instruction(&Instruction::LocalSet(layout.ram_base_local()));

            func.instruction(&Instruction::LocalGet(layout.jit_ctx_ptr_local()));
            func.instruction(&Instruction::I64Load(memarg(
                JitContext::TLB_SALT_OFFSET,
                3,
            )));
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
            if state_usage.gpr_written[gpr.as_u8() as usize] {
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
        }

        if state_usage.rflags_written {
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
        }

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

    fn code_version_table_ptr_local(self) -> u32 {
        self.next_rip_local() + 1
    }

    fn code_version_table_len_local(self) -> u32 {
        self.code_version_table_ptr_local() + 1
    }

    fn ram_base_local(self) -> u32 {
        self.code_version_table_len_local() + 1
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
        // gpr[16] + rip + rflags + next_rip + code_version_table ptr/len + ram_base + tlb_salt +
        // scratch locals (vaddr, vpn, tlb_data, scratch) + values
        16 + 1 + 1 + 1 + 2 + 1 + 1 + 4 + self.values
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
    fn inline_tlb_locals(&self) -> InlineTlbLocals {
        InlineTlbLocals {
            cpu_ptr: self.layout.cpu_ptr_local(),
            jit_ctx_ptr: self.layout.jit_ctx_ptr_local(),
            ram_base: self.layout.ram_base_local(),
            tlb_salt: self.layout.tlb_salt_local(),
            scratch_vaddr: self.layout.scratch_vaddr_local(),
            scratch_vpn: self.layout.scratch_vpn_local(),
            scratch_tlb_data: self.layout.scratch_tlb_data_local(),
            code_version_table_ptr: Some(self.layout.code_version_table_ptr_local()),
            code_version_table_len: Some(self.layout.code_version_table_len_local()),
        }
    }

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
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_read_u8
                                    .expect("mem_read_u8 import missing"),
                            ));
                            self.func.instruction(&Instruction::I64ExtendI32U);
                        }
                        Width::W16 => {
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_read_u16
                                    .expect("mem_read_u16 import missing"),
                            ));
                            self.func.instruction(&Instruction::I64ExtendI32U);
                        }
                        Width::W32 => {
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_read_u32
                                    .expect("mem_read_u32 import missing"),
                            ));
                            self.func.instruction(&Instruction::I64ExtendI32U);
                        }
                        Width::W64 => {
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_read_u64
                                    .expect("mem_read_u64 import missing"),
                            ));
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
                let slow_read = slow_read.expect("memory read helper import missing");

                // Cross-page accesses default to the slow helper for correctness unless the
                // optional split-access fast-path is enabled.
                let cross_limit =
                    crate::PAGE_OFFSET_MASK.saturating_sub(size_bytes.saturating_sub(1) as u64);
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
                    if self.options.inline_tlb_cross_page_fastpath
                        && matches!(*width, Width::W16 | Width::W32 | Width::W64)
                    {
                        // Cross-page fast-path: translate both pages, ensure both are RAM, then do
                        // two direct same-page loads and combine the result (little-endian).

                        // shift_bytes = (vaddr & 0xFFF) - (PAGE_SIZE - size_bytes)
                        let page_tail = crate::PAGE_SIZE.saturating_sub(size_bytes as u64);
                        self.func
                            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                        self.func
                            .instruction(&Instruction::I64Const(crate::PAGE_OFFSET_MASK as i64));
                        self.func.instruction(&Instruction::I64And);
                        self.func
                            .instruction(&Instruction::I64Const(page_tail as i64));
                        self.func.instruction(&Instruction::I64Sub);
                        self.func
                            .instruction(&Instruction::LocalSet(self.layout.scratch_local()));

                        let emit_split_load = |this: &mut Self| {
                            // part0 = loadN(vaddr - shift_bytes) >> (shift_bytes * 8)
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.value_local(*addr)));
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.scratch_local()));
                            this.func.instruction(&Instruction::I64Sub);
                            this.func.instruction(&Instruction::LocalSet(
                                this.layout.scratch_vaddr_local(),
                            ));

                            this.emit_translate_and_cache(MMU_ACCESS_READ, crate::TLB_FLAG_READ);
                            this.emit_compute_ram_addr();
                            match *width {
                                Width::W16 => this
                                    .func
                                    .instruction(&Instruction::I64Load16U(memarg(0, 1))),
                                Width::W32 => this
                                    .func
                                    .instruction(&Instruction::I64Load32U(memarg(0, 2))),
                                Width::W64 => this.func.instruction(&Instruction::I64Load(memarg(0, 3))),
                                _ => unreachable!(),
                            };
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.scratch_local()));
                            this.func.instruction(&Instruction::I64Const(8));
                            this.func.instruction(&Instruction::I64Mul);
                            this.func.instruction(&Instruction::I64ShrU);
                            // Stash part0 in `dst`'s local as a temporary.
                            this.func
                                .instruction(&Instruction::LocalSet(this.layout.value_local(*dst)));

                            // part1 = (loadN(vaddr1) & ((1 << (shift_bytes * 8)) - 1))
                            //           << ((size_bytes * 8) - (shift_bytes * 8))
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.value_local(*addr)));
                            this.func
                                .instruction(&Instruction::I64Const(size_bytes as i64));
                            this.func.instruction(&Instruction::I64Add);
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.scratch_local()));
                            this.func.instruction(&Instruction::I64Sub);
                            this.func.instruction(&Instruction::LocalSet(
                                this.layout.scratch_vaddr_local(),
                            ));

                            this.emit_translate_and_cache(MMU_ACCESS_READ, crate::TLB_FLAG_READ);
                            this.emit_compute_ram_addr();
                            match *width {
                                Width::W16 => this
                                    .func
                                    .instruction(&Instruction::I64Load16U(memarg(0, 1))),
                                Width::W32 => this
                                    .func
                                    .instruction(&Instruction::I64Load32U(memarg(0, 2))),
                                Width::W64 => this.func.instruction(&Instruction::I64Load(memarg(0, 3))),
                                _ => unreachable!(),
                            };

                            // mask = (1 << (shift_bytes*8)) - 1
                            this.func.instruction(&Instruction::I64Const(1));
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.scratch_local()));
                            this.func.instruction(&Instruction::I64Const(8));
                            this.func.instruction(&Instruction::I64Mul);
                            this.func.instruction(&Instruction::I64Shl);
                            this.func.instruction(&Instruction::I64Const(1));
                            this.func.instruction(&Instruction::I64Sub);
                            this.func.instruction(&Instruction::I64And);

                            // << ((size_bytes*8) - (shift_bytes*8))
                            this.func
                                .instruction(&Instruction::I64Const((size_bytes * 8) as i64));
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.scratch_local()));
                            this.func.instruction(&Instruction::I64Const(8));
                            this.func.instruction(&Instruction::I64Mul);
                            this.func.instruction(&Instruction::I64Sub);
                            this.func.instruction(&Instruction::I64Shl);

                            // part0 | part1
                            this.func
                                .instruction(&Instruction::LocalGet(this.layout.value_local(*dst)));
                            this.func.instruction(&Instruction::I64Or);
                            this.emit_trunc(*width);
                            this.func
                                .instruction(&Instruction::LocalSet(this.layout.value_local(*dst)));
                        };

                         // Page 0: translate.
                         self.emit_translate_and_cache(MMU_ACCESS_READ, crate::TLB_FLAG_READ);

                         if self.options.inline_tlb_mmio_exit {
                            self.emit_mmio_exit(size_bytes, 0, None);

                            // Page 1: translate and ensure RAM. Use the original vaddr for the MMIO
                            // exit payload (even though we probe the second page).
                            //
                            // vaddr1 = vaddr + size_bytes - shift_bytes
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.value_local(*addr),
                            ));
                            self.func
                                .instruction(&Instruction::I64Const(size_bytes as i64));
                            self.func.instruction(&Instruction::I64Add);
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.scratch_local(),
                            ));
                            self.func.instruction(&Instruction::I64Sub);
                            self.func.instruction(&Instruction::LocalSet(
                                self.layout.scratch_vaddr_local(),
                            ));

                            self.emit_translate_and_cache(MMU_ACCESS_READ, crate::TLB_FLAG_READ);

                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.value_local(*addr),
                            ));
                            self.func.instruction(&Instruction::LocalSet(
                                self.layout.scratch_vaddr_local(),
                            ));
                            self.emit_mmio_exit(size_bytes, 0, None);

                            emit_split_load(self);
                        } else {
                            // If either page isn't backed by RAM, fall back to the slow helper and
                            // keep executing this block.
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.scratch_tlb_data_local(),
                            ));
                            self.func
                                .instruction(&Instruction::I64Const(crate::TLB_FLAG_IS_RAM as i64));
                            self.func.instruction(&Instruction::I64And);
                            self.func.instruction(&Instruction::I64Eqz);

                            self.func.instruction(&Instruction::If(BlockType::Empty));
                            self.depth += 1;
                            {
                                // Slow path.
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.cpu_ptr_local(),
                                ));
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.scratch_vaddr_local(),
                                ));
                                self.func.instruction(&Instruction::Call(slow_read));
                                if !matches!(*width, Width::W64) {
                                    self.func.instruction(&Instruction::I64ExtendI32U);
                                }
                                self.emit_trunc(*width);
                                self.func.instruction(&Instruction::LocalSet(
                                    self.layout.value_local(*dst),
                                ));
                            }
                            self.func.instruction(&Instruction::Else);
                            {
                                // Translate page 1.
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.value_local(*addr),
                                ));
                                self.func
                                    .instruction(&Instruction::I64Const(size_bytes as i64));
                                self.func.instruction(&Instruction::I64Add);
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.scratch_local(),
                                ));
                                self.func.instruction(&Instruction::I64Sub);
                                self.func.instruction(&Instruction::LocalSet(
                                    self.layout.scratch_vaddr_local(),
                                ));

                                self.emit_translate_and_cache(
                                    MMU_ACCESS_READ,
                                    crate::TLB_FLAG_READ,
                                );

                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.scratch_tlb_data_local(),
                                ));
                                self.func.instruction(&Instruction::I64Const(
                                    crate::TLB_FLAG_IS_RAM as i64,
                                ));
                                self.func.instruction(&Instruction::I64And);
                                self.func.instruction(&Instruction::I64Eqz);

                                self.func.instruction(&Instruction::If(BlockType::Empty));
                                self.depth += 1;
                                {
                                    // Slow path (restore original vaddr first).
                                    self.func.instruction(&Instruction::LocalGet(
                                        self.layout.value_local(*addr),
                                    ));
                                    self.func.instruction(&Instruction::LocalSet(
                                        self.layout.scratch_vaddr_local(),
                                    ));

                                    self.func.instruction(&Instruction::LocalGet(
                                        self.layout.cpu_ptr_local(),
                                    ));
                                    self.func.instruction(&Instruction::LocalGet(
                                        self.layout.scratch_vaddr_local(),
                                    ));
                                    self.func.instruction(&Instruction::Call(slow_read));
                                    if !matches!(*width, Width::W64) {
                                        self.func.instruction(&Instruction::I64ExtendI32U);
                                    }
                                    self.emit_trunc(*width);
                                    self.func.instruction(&Instruction::LocalSet(
                                        self.layout.value_local(*dst),
                                    ));
                                }
                                self.func.instruction(&Instruction::Else);
                                {
                                    emit_split_load(self);
                                }
                                self.func.instruction(&Instruction::End);
                                self.depth -= 1;
                            }
                            self.func.instruction(&Instruction::End);
                            self.depth -= 1;
                        }
                    } else {
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
                }
                self.func.instruction(&Instruction::Else);
                {
                    // Fast path: inline TLB probe + direct RAM load.
                    self.emit_translate_and_cache(MMU_ACCESS_READ, crate::TLB_FLAG_READ);

                    if self.options.inline_tlb_mmio_exit {
                        self.emit_mmio_exit(size_bytes, 0, None);

                        self.emit_compute_ram_addr();
                        match *width {
                            Width::W8 => {
                                self.func.instruction(&Instruction::I64Load8U(memarg(0, 0)))
                            }
                            Width::W16 => self
                                .func
                                .instruction(&Instruction::I64Load16U(memarg(0, 1))),
                            Width::W32 => self
                                .func
                                .instruction(&Instruction::I64Load32U(memarg(0, 2))),
                            Width::W64 => {
                                self.func.instruction(&Instruction::I64Load(memarg(0, 3)))
                            }
                        };
                        self.emit_trunc(*width);
                        self.func
                            .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
                    } else {
                        // If the translation isn't backed by RAM (MMIO/ROM/unmapped), fall back to
                        // the slow helper and keep executing this block.
                        self.func.instruction(&Instruction::LocalGet(
                            self.layout.scratch_tlb_data_local(),
                        ));
                        self.func
                            .instruction(&Instruction::I64Const(crate::TLB_FLAG_IS_RAM as i64));
                        self.func.instruction(&Instruction::I64And);
                        self.func.instruction(&Instruction::I64Eqz);

                        self.func.instruction(&Instruction::If(BlockType::Empty));
                        self.depth += 1;
                        {
                            // Slow path.
                            self.func
                                .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.scratch_vaddr_local(),
                            ));
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
                            // RAM fast-path.
                            self.emit_compute_ram_addr();
                            match *width {
                                Width::W8 => {
                                    self.func.instruction(&Instruction::I64Load8U(memarg(0, 0)))
                                }
                                Width::W16 => self
                                    .func
                                    .instruction(&Instruction::I64Load16U(memarg(0, 1))),
                                Width::W32 => self
                                    .func
                                    .instruction(&Instruction::I64Load32U(memarg(0, 2))),
                                Width::W64 => {
                                    self.func.instruction(&Instruction::I64Load(memarg(0, 3)))
                                }
                            };
                            self.emit_trunc(*width);
                            self.func
                                .instruction(&Instruction::LocalSet(self.layout.value_local(*dst)));
                        }
                        self.func.instruction(&Instruction::End);
                        self.depth -= 1;
                    }
                }
                self.func.instruction(&Instruction::End);
                self.depth -= 1;
            }
            IrInst::Store { addr, src, width } => {
                if !self.options.inline_tlb || !self.options.inline_tlb_stores {
                    // Slow path: always go through the imported helpers.
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
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_write_u8
                                    .expect("mem_write_u8 import missing"),
                            ));
                        }
                        Width::W16 => {
                            self.emit_trunc(Width::W16);
                            self.func.instruction(&Instruction::I32WrapI64);
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_write_u16
                                    .expect("mem_write_u16 import missing"),
                            ));
                        }
                        Width::W32 => {
                            self.emit_trunc(Width::W32);
                            self.func.instruction(&Instruction::I32WrapI64);
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_write_u32
                                    .expect("mem_write_u32 import missing"),
                            ));
                        }
                        Width::W64 => {
                            self.func.instruction(&Instruction::Call(
                                self.imported
                                    .mem_write_u64
                                    .expect("mem_write_u64 import missing"),
                            ));
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
                let slow_write = slow_write.expect("memory write helper import missing");

                let cross_limit =
                    crate::PAGE_OFFSET_MASK.saturating_sub(size_bytes.saturating_sub(1) as u64);
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
                    if self.options.inline_tlb_cross_page_fastpath
                        && matches!(*width, Width::W16 | Width::W32 | Width::W64)
                    {
                        // Cross-page fast-path: translate both pages, ensure both are RAM, then
                        // split the store into per-page writes (little-endian).

                        // shift_bytes = (vaddr & 0xFFF) - (PAGE_SIZE - size_bytes)
                        let page_tail = crate::PAGE_SIZE.saturating_sub(size_bytes as u64);
                        self.func
                            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                        self.func
                            .instruction(&Instruction::I64Const(crate::PAGE_OFFSET_MASK as i64));
                        self.func.instruction(&Instruction::I64And);
                        self.func
                            .instruction(&Instruction::I64Const(page_tail as i64));
                        self.func.instruction(&Instruction::I64Sub);
                        self.func
                            .instruction(&Instruction::LocalSet(self.layout.scratch_local()));

                        let emit_split_store_and_bump = |this: &mut Self| {
                            // After confirming both pages are RAM, emit a per-chunk split store. The
                            // additional inline-TLB probes in these chunks should hit the already
                            // cached entries in the common case, while keeping the codegen simple.

                            let addr_local = this.layout.value_local(*addr);
                            let src_local = this.layout.value_local(*src);
                            let shift_local = this.layout.scratch_local();
                            let scratch_vaddr_local = this.layout.scratch_vaddr_local();

                            let emit_store_page0_chunk =
                                |this: &mut Self, mem_off: u32, nbytes: u32, shift_bits: u32| {
                                    // scratch_vaddr = vaddr + mem_off
                                    this.func.instruction(&Instruction::LocalGet(addr_local));
                                    if mem_off != 0 {
                                        this.func
                                            .instruction(&Instruction::I64Const(mem_off as i64));
                                        this.func.instruction(&Instruction::I64Add);
                                    }
                                    this.func
                                        .instruction(&Instruction::LocalSet(scratch_vaddr_local));

                                    this.emit_translate_and_cache(
                                        MMU_ACCESS_WRITE,
                                        crate::TLB_FLAG_WRITE,
                                    );
                                    this.emit_compute_ram_addr();
                                    this.func.instruction(&Instruction::LocalGet(src_local));
                                    if shift_bits != 0 {
                                        this.func
                                            .instruction(&Instruction::I64Const(shift_bits as i64));
                                        this.func.instruction(&Instruction::I64ShrU);
                                    }
                                    match nbytes {
                                        1 => this
                                            .func
                                            .instruction(&Instruction::I64Store8(memarg(0, 0))),
                                        2 => this
                                            .func
                                            .instruction(&Instruction::I64Store16(memarg(0, 1))),
                                        4 => this
                                            .func
                                            .instruction(&Instruction::I64Store32(memarg(0, 2))),
                                        _ => unreachable!("invalid store chunk size: {nbytes}"),
                                    };
                                };

                            let emit_store_page1_chunk =
                                |this: &mut Self, mem_off: u32, nbytes: u32, shift_bits: u32| {
                                    // scratch_vaddr = vaddr + size_bytes - shift_bytes + mem_off
                                    this.func.instruction(&Instruction::LocalGet(addr_local));
                                    this.func
                                        .instruction(&Instruction::I64Const(size_bytes as i64));
                                    this.func.instruction(&Instruction::I64Add);
                                    this.func.instruction(&Instruction::LocalGet(shift_local));
                                    this.func.instruction(&Instruction::I64Sub);
                                    if mem_off != 0 {
                                        this.func
                                            .instruction(&Instruction::I64Const(mem_off as i64));
                                        this.func.instruction(&Instruction::I64Add);
                                    }
                                    this.func
                                        .instruction(&Instruction::LocalSet(scratch_vaddr_local));

                                    this.emit_translate_and_cache(
                                        MMU_ACCESS_WRITE,
                                        crate::TLB_FLAG_WRITE,
                                    );
                                    this.emit_compute_ram_addr();
                                    this.func.instruction(&Instruction::LocalGet(src_local));
                                    if shift_bits != 0 {
                                        this.func
                                            .instruction(&Instruction::I64Const(shift_bits as i64));
                                        this.func.instruction(&Instruction::I64ShrU);
                                    }
                                     match nbytes {
                                         1 => this
                                             .func
                                             .instruction(&Instruction::I64Store8(memarg(0, 0))),
                                         2 => this
                                            .func
                                            .instruction(&Instruction::I64Store16(memarg(0, 1))),
                                         4 => this
                                             .func
                                             .instruction(&Instruction::I64Store32(memarg(0, 2))),
                                         _ => unreachable!("invalid store chunk size: {nbytes}"),
                                     };
                                 };

                            // Dispatch based on `shift_bytes` (bytes written to page 1). Note that in
                            // the cross-page case `shift_bytes` is always in the range [1, size_bytes).
                            match *width {
                                Width::W16 => {
                                    emit_store_page0_chunk(this, 0, 1, 0);
                                    emit_store_page1_chunk(this, 0, 1, 8);
                                }
                                Width::W32 => {
                                    this.func.instruction(&Instruction::LocalGet(shift_local));
                                    this.func.instruction(&Instruction::I64Const(1));
                                    this.func.instruction(&Instruction::I64Eq);
                                    this.func.instruction(&Instruction::If(BlockType::Empty));
                                    {
                                        // n1=3, n2=1
                                        emit_store_page0_chunk(this, 0, 2, 0);
                                        emit_store_page0_chunk(this, 2, 1, 16);
                                        emit_store_page1_chunk(this, 0, 1, 24);
                                    }
                                    this.func.instruction(&Instruction::Else);
                                    {
                                        this.func.instruction(&Instruction::LocalGet(shift_local));
                                        this.func.instruction(&Instruction::I64Const(2));
                                        this.func.instruction(&Instruction::I64Eq);
                                        this.func.instruction(&Instruction::If(BlockType::Empty));
                                        {
                                            // n1=2, n2=2
                                            emit_store_page0_chunk(this, 0, 2, 0);
                                            emit_store_page1_chunk(this, 0, 2, 16);
                                        }
                                        this.func.instruction(&Instruction::Else);
                                        {
                                            // n1=1, n2=3
                                            emit_store_page0_chunk(this, 0, 1, 0);
                                            emit_store_page1_chunk(this, 0, 2, 8);
                                            emit_store_page1_chunk(this, 2, 1, 24);
                                        }
                                        this.func.instruction(&Instruction::End);
                                    }
                                    this.func.instruction(&Instruction::End);
                                }
                                Width::W64 => {
                                    this.func.instruction(&Instruction::LocalGet(shift_local));
                                    this.func.instruction(&Instruction::I64Const(1));
                                    this.func.instruction(&Instruction::I64Eq);
                                    this.func.instruction(&Instruction::If(BlockType::Empty));
                                    {
                                        // n1=7, n2=1
                                        emit_store_page0_chunk(this, 0, 4, 0);
                                        emit_store_page0_chunk(this, 4, 2, 32);
                                        emit_store_page0_chunk(this, 6, 1, 48);
                                        emit_store_page1_chunk(this, 0, 1, 56);
                                    }
                                    this.func.instruction(&Instruction::Else);
                                    {
                                        this.func.instruction(&Instruction::LocalGet(shift_local));
                                        this.func.instruction(&Instruction::I64Const(2));
                                        this.func.instruction(&Instruction::I64Eq);
                                        this.func.instruction(&Instruction::If(BlockType::Empty));
                                        {
                                            // n1=6, n2=2
                                            emit_store_page0_chunk(this, 0, 4, 0);
                                            emit_store_page0_chunk(this, 4, 2, 32);
                                            emit_store_page1_chunk(this, 0, 2, 48);
                                        }
                                        this.func.instruction(&Instruction::Else);
                                        {
                                            this.func.instruction(&Instruction::LocalGet(shift_local));
                                            this.func.instruction(&Instruction::I64Const(3));
                                            this.func.instruction(&Instruction::I64Eq);
                                            this.func.instruction(&Instruction::If(BlockType::Empty));
                                            {
                                                // n1=5, n2=3
                                                emit_store_page0_chunk(this, 0, 4, 0);
                                                emit_store_page0_chunk(this, 4, 1, 32);
                                                emit_store_page1_chunk(this, 0, 2, 40);
                                                emit_store_page1_chunk(this, 2, 1, 56);
                                            }
                                            this.func.instruction(&Instruction::Else);
                                            {
                                                this.func.instruction(&Instruction::LocalGet(
                                                    shift_local,
                                                ));
                                                this.func.instruction(&Instruction::I64Const(4));
                                                this.func.instruction(&Instruction::I64Eq);
                                                this.func.instruction(&Instruction::If(
                                                    BlockType::Empty,
                                                ));
                                                {
                                                    // n1=4, n2=4
                                                    emit_store_page0_chunk(this, 0, 4, 0);
                                                    emit_store_page1_chunk(this, 0, 4, 32);
                                                }
                                                this.func.instruction(&Instruction::Else);
                                                {
                                                    this.func.instruction(&Instruction::LocalGet(
                                                        shift_local,
                                                    ));
                                                    this.func.instruction(&Instruction::I64Const(5));
                                                    this.func.instruction(&Instruction::I64Eq);
                                                    this.func.instruction(&Instruction::If(
                                                        BlockType::Empty,
                                                    ));
                                                    {
                                                        // n1=3, n2=5
                                                        emit_store_page0_chunk(this, 0, 2, 0);
                                                        emit_store_page0_chunk(this, 2, 1, 16);
                                                        emit_store_page1_chunk(this, 0, 4, 24);
                                                        emit_store_page1_chunk(this, 4, 1, 56);
                                                    }
                                                    this.func.instruction(&Instruction::Else);
                                                    {
                                                        this.func.instruction(&Instruction::LocalGet(
                                                            shift_local,
                                                        ));
                                                        this.func
                                                            .instruction(&Instruction::I64Const(6));
                                                        this.func.instruction(&Instruction::I64Eq);
                                                        this.func.instruction(&Instruction::If(
                                                            BlockType::Empty,
                                                        ));
                                                        {
                                                            // n1=2, n2=6
                                                            emit_store_page0_chunk(this, 0, 2, 0);
                                                            emit_store_page1_chunk(this, 0, 4, 16);
                                                            emit_store_page1_chunk(this, 4, 2, 48);
                                                        }
                                                        this.func.instruction(&Instruction::Else);
                                                        {
                                                            // n1=1, n2=7
                                                            emit_store_page0_chunk(this, 0, 1, 0);
                                                            emit_store_page1_chunk(this, 0, 4, 8);
                                                            emit_store_page1_chunk(this, 4, 2, 40);
                                                            emit_store_page1_chunk(this, 6, 1, 56);
                                                        }
                                                        this.func.instruction(&Instruction::End);
                                                    }
                                                    this.func.instruction(&Instruction::End);
                                                }
                                                this.func.instruction(&Instruction::End);
                                            }
                                            this.func.instruction(&Instruction::End);
                                        }
                                        this.func.instruction(&Instruction::End);
                                    }
                                    this.func.instruction(&Instruction::End);
                                }
                                _ => unreachable!(),
                            }

                            // Self-modifying code invalidation: bump the version entry for both written
                            // physical pages.
                            //
                            // Re-translate each page to get the correct physical base for the bump.
                            this.func.instruction(&Instruction::LocalGet(
                                this.layout.value_local(*addr),
                            ));
                            this.func.instruction(&Instruction::LocalSet(
                                this.layout.scratch_vaddr_local(),
                            ));
                            this.emit_translate_and_cache(
                                MMU_ACCESS_WRITE,
                                crate::TLB_FLAG_WRITE,
                            );
                            this.emit_bump_code_version_fastpath();

                            this.func.instruction(&Instruction::LocalGet(
                                this.layout.value_local(*addr),
                            ));
                            this.func
                                .instruction(&Instruction::I64Const(size_bytes as i64));
                            this.func.instruction(&Instruction::I64Add);
                            this.func.instruction(&Instruction::LocalGet(
                                this.layout.scratch_local(),
                            ));
                            this.func.instruction(&Instruction::I64Sub);
                            this.func.instruction(&Instruction::LocalSet(
                                this.layout.scratch_vaddr_local(),
                            ));
                            this.emit_translate_and_cache(
                                MMU_ACCESS_WRITE,
                                crate::TLB_FLAG_WRITE,
                            );
                            this.emit_bump_code_version_fastpath();
                        };

                        // Page 0: translate.
                        self.emit_translate_and_cache(MMU_ACCESS_WRITE, crate::TLB_FLAG_WRITE);

                        if self.options.inline_tlb_mmio_exit {
                            self.emit_mmio_exit(size_bytes, 1, Some(*src));

                            // Translate page 1 (but use original vaddr in the exit payload).
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.value_local(*addr),
                            ));
                            self.func
                                .instruction(&Instruction::I64Const(size_bytes as i64));
                            self.func.instruction(&Instruction::I64Add);
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.scratch_local(),
                            ));
                            self.func.instruction(&Instruction::I64Sub);
                            self.func.instruction(&Instruction::LocalSet(
                                self.layout.scratch_vaddr_local(),
                            ));

                            self.emit_translate_and_cache(MMU_ACCESS_WRITE, crate::TLB_FLAG_WRITE);

                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.value_local(*addr),
                            ));
                            self.func.instruction(&Instruction::LocalSet(
                                self.layout.scratch_vaddr_local(),
                            ));
                            self.emit_mmio_exit(size_bytes, 1, Some(*src));

                            emit_split_store_and_bump(self);
                        } else {
                            // If either page isn't backed by RAM, fall back to the slow helper and
                            // keep executing this block.
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.scratch_tlb_data_local(),
                            ));
                            self.func
                                .instruction(&Instruction::I64Const(crate::TLB_FLAG_IS_RAM as i64));
                            self.func.instruction(&Instruction::I64And);
                            self.func.instruction(&Instruction::I64Eqz);

                            self.func.instruction(&Instruction::If(BlockType::Empty));
                            self.depth += 1;
                            {
                                // Slow path.
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.cpu_ptr_local(),
                                ));
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.scratch_vaddr_local(),
                                ));
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.value_local(*src),
                                ));
                                if !matches!(*width, Width::W64) {
                                    self.emit_trunc(*width);
                                    self.func.instruction(&Instruction::I32WrapI64);
                                }
                                self.func.instruction(&Instruction::Call(slow_write));
                            }
                            self.func.instruction(&Instruction::Else);
                            {
                                // Translate page 1.
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.value_local(*addr),
                                ));
                                self.func
                                    .instruction(&Instruction::I64Const(size_bytes as i64));
                                self.func.instruction(&Instruction::I64Add);
                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.scratch_local(),
                                ));
                                self.func.instruction(&Instruction::I64Sub);
                                self.func.instruction(&Instruction::LocalSet(
                                    self.layout.scratch_vaddr_local(),
                                ));

                                self.emit_translate_and_cache(
                                    MMU_ACCESS_WRITE,
                                    crate::TLB_FLAG_WRITE,
                                );

                                self.func.instruction(&Instruction::LocalGet(
                                    self.layout.scratch_tlb_data_local(),
                                ));
                                self.func.instruction(&Instruction::I64Const(
                                    crate::TLB_FLAG_IS_RAM as i64,
                                ));
                                self.func.instruction(&Instruction::I64And);
                                self.func.instruction(&Instruction::I64Eqz);

                                self.func.instruction(&Instruction::If(BlockType::Empty));
                                self.depth += 1;
                                {
                                    // Slow path (restore original vaddr first).
                                    self.func.instruction(&Instruction::LocalGet(
                                        self.layout.value_local(*addr),
                                    ));
                                    self.func.instruction(&Instruction::LocalSet(
                                        self.layout.scratch_vaddr_local(),
                                    ));

                                    self.func.instruction(&Instruction::LocalGet(
                                        self.layout.cpu_ptr_local(),
                                    ));
                                    self.func.instruction(&Instruction::LocalGet(
                                        self.layout.scratch_vaddr_local(),
                                    ));
                                    self.func.instruction(&Instruction::LocalGet(
                                        self.layout.value_local(*src),
                                    ));
                                    if !matches!(*width, Width::W64) {
                                        self.emit_trunc(*width);
                                        self.func.instruction(&Instruction::I32WrapI64);
                                    }
                                    self.func.instruction(&Instruction::Call(slow_write));
                                }
                                self.func.instruction(&Instruction::Else);
                                {
                                    emit_split_store_and_bump(self);
                                }
                                self.func.instruction(&Instruction::End);
                                self.depth -= 1;
                            }
                            self.func.instruction(&Instruction::End);
                            self.depth -= 1;
                        }
                    } else {
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
                }
                self.func.instruction(&Instruction::Else);
                {
                    // Fast path: inline TLB probe + direct RAM store.
                    self.emit_translate_and_cache(MMU_ACCESS_WRITE, crate::TLB_FLAG_WRITE);

                    if self.options.inline_tlb_mmio_exit {
                        self.emit_mmio_exit(size_bytes, 1, Some(*src));

                        self.emit_compute_ram_addr();
                        self.func
                            .instruction(&Instruction::LocalGet(self.layout.value_local(*src)));
                        match *width {
                            Width::W8 => {
                                self.func.instruction(&Instruction::I64Store8(memarg(0, 0)))
                            }
                            Width::W16 => self
                                .func
                                .instruction(&Instruction::I64Store16(memarg(0, 1))),
                            Width::W32 => self
                                .func
                                .instruction(&Instruction::I64Store32(memarg(0, 2))),
                            Width::W64 => {
                                self.func.instruction(&Instruction::I64Store(memarg(0, 3)))
                            }
                        };

                        // Self-modifying code invalidation: bump the version entry for the written
                        // physical page. We conservatively bump for all RAM writes.
                        self.emit_bump_code_version_fastpath();
                    } else {
                        // If the translation isn't backed by RAM (MMIO/ROM/unmapped), fall back to
                        // the slow helper and keep executing this block.
                        self.func.instruction(&Instruction::LocalGet(
                            self.layout.scratch_tlb_data_local(),
                        ));
                        self.func
                            .instruction(&Instruction::I64Const(crate::TLB_FLAG_IS_RAM as i64));
                        self.func.instruction(&Instruction::I64And);
                        self.func.instruction(&Instruction::I64Eqz);

                        self.func.instruction(&Instruction::If(BlockType::Empty));
                        self.depth += 1;
                        {
                            // Slow path.
                            self.func
                                .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                            self.func.instruction(&Instruction::LocalGet(
                                self.layout.scratch_vaddr_local(),
                            ));
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
                            // RAM fast-path.
                            self.emit_compute_ram_addr();
                            self.func
                                .instruction(&Instruction::LocalGet(self.layout.value_local(*src)));
                            match *width {
                                Width::W8 => {
                                    self.func.instruction(&Instruction::I64Store8(memarg(0, 0)))
                                }
                                Width::W16 => self
                                    .func
                                    .instruction(&Instruction::I64Store16(memarg(0, 1))),
                                Width::W32 => self
                                    .func
                                    .instruction(&Instruction::I64Store32(memarg(0, 2))),
                                Width::W64 => {
                                    self.func.instruction(&Instruction::I64Store(memarg(0, 3)))
                                }
                            };

                            // Self-modifying code invalidation: bump the version entry for the
                            // written physical page. We conservatively bump for all RAM writes.
                            self.emit_bump_code_version_fastpath();
                        }
                        self.func.instruction(&Instruction::End);
                        self.depth -= 1;
                    }
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
                        BinOp::Shl | BinOp::Shr | BinOp::Sar => {
                            self.emit_shift_flags(*op, *width, *flags, *lhs, *rhs, *dst)
                        }
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
            IrInst::CallHelper { .. } => {
                // Tier-1 blocks currently do not have deopt metadata for resuming mid-block. Treat
                // helper calls as a runtime bailout and request a one-shot interpreter fallback.
                //
                // Higher-level compilation helpers may reject helper calls up-front, but keep the
                // codegen itself defensive so direct users of `Tier1WasmCodegen` don't hit a hard
                // panic at compile time.
                //
                // Mark this as a runtime exit so the backend can roll back state if needed.
                // `kind` is currently unused by the host stub helpers, so use 0.
                self.func.instruction(&Instruction::I32Const(0));
                self.func
                    .instruction(&Instruction::LocalGet(self.layout.rip_local()));
                self.func.instruction(&Instruction::Call(
                    self.imported.jit_exit.expect("jit_exit import missing"),
                ));
                // `jit_exit` returns the RIP to resume at while we use the sentinel return value to
                // request an interpreter step.
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
                self.func
                    .instruction(&Instruction::I64Const(JIT_EXIT_SENTINEL_I64));
                self.func
                    .instruction(&Instruction::LocalSet(self.layout.scratch_local()));
                self.func.instruction(&Instruction::Br(self.depth));
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
        let locals = self.inline_tlb_locals();
        let mmu_translate = self
            .imported
            .mmu_translate
            .expect("mmu_translate import missing");
        inline_tlb_codegen::emit_translate_and_cache(
            self.func,
            &mut self.depth,
            locals,
            mmu_translate,
            access_code,
            required_flag,
        );
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
        let locals = self.inline_tlb_locals();
        inline_tlb_codegen::emit_compute_ram_addr(self.func, locals);
    }

    /// Bumps the page-version entry for the current RAM write (inline fast-path stores only).
    ///
    /// The runtime may choose to only bump for pages marked as executable/code, but for initial
    /// correctness we bump for all writes that hit RAM.
    fn emit_bump_code_version_fastpath(&mut self) {
        let locals = self.inline_tlb_locals();
        inline_tlb_codegen::emit_bump_code_version_fastpath(
            self.func,
            locals,
            self.options.memory_shared,
        );
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
        // x86 shifts mask the count to 5 bits for 8/16/32-bit operands and 6 bits for 64-bit.
        // Note: this differs from WASM's built-in masking (which always uses 6 bits for i64
        // shifts), so we must apply the x86 mask explicitly for narrow widths.
        let mask = if width == Width::W64 { 63 } else { 31 };
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

    fn emit_shift_flags(
        &mut self,
        op: BinOp,
        width: Width,
        flags: FlagSet,
        lhs: ValueId,
        rhs: ValueId,
        res: ValueId,
    ) {
        debug_assert!(matches!(op, BinOp::Shl | BinOp::Shr | BinOp::Sar));

        // x86 shifts do not update any flags when the (masked) shift count is 0.
        //
        // Note: AF is architecturally undefined for shifts. We conservatively leave it unchanged
        // even if requested.
        let sign_bit = 1u64 << (width.bits() - 1);
        let lhs_local = self.layout.value_local(lhs);
        let rhs_local = self.layout.value_local(rhs);
        let res_local = self.layout.value_local(res);
        let amt_local = self.layout.scratch_local();

        // amt = rhs & shift_mask
        self.func.instruction(&Instruction::LocalGet(rhs_local));
        self.emit_shift_mask(width);
        self.func.instruction(&Instruction::LocalTee(amt_local));

        // if amt != 0 { ... }
        self.func.instruction(&Instruction::I64Eqz);
        self.func.instruction(&Instruction::I32Eqz);
        self.func.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
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

            // CF is only defined for shift counts in the range [1, width.bits()]. For larger
            // counts (possible for 8/16-bit operands due to x86's 5-bit masking), CF is undefined;
            // conservatively leave it unchanged.
            if flags.contains(FlagSet::CF) {
                self.func.instruction(&Instruction::LocalGet(amt_local));
                self.func
                    .instruction(&Instruction::I64Const(width.bits() as i64));
                self.func.instruction(&Instruction::I64LeU);
                self.func.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                {
                    self.emit_set_flag(Flag::Cf, |this| match op {
                        BinOp::Shl => {
                            // (lhs >> (bits - amt)) & 1
                            this.func.instruction(&Instruction::LocalGet(lhs_local));
                            this.func
                                .instruction(&Instruction::I64Const(width.bits() as i64));
                            this.func.instruction(&Instruction::LocalGet(amt_local));
                            this.func.instruction(&Instruction::I64Sub);
                            this.func.instruction(&Instruction::I64ShrU);
                            this.func.instruction(&Instruction::I64Const(1));
                            this.func.instruction(&Instruction::I64And);
                            this.func.instruction(&Instruction::I64Const(0));
                            this.func.instruction(&Instruction::I64Ne);
                        }
                        BinOp::Shr | BinOp::Sar => {
                            // (lhs >> (amt - 1)) & 1
                            this.func.instruction(&Instruction::LocalGet(lhs_local));
                            this.func.instruction(&Instruction::LocalGet(amt_local));
                            this.func.instruction(&Instruction::I64Const(1));
                            this.func.instruction(&Instruction::I64Sub);
                            this.func.instruction(&Instruction::I64ShrU);
                            this.func.instruction(&Instruction::I64Const(1));
                            this.func.instruction(&Instruction::I64And);
                            this.func.instruction(&Instruction::I64Const(0));
                            this.func.instruction(&Instruction::I64Ne);
                        }
                        _ => unreachable!(),
                    });
                }
                self.func.instruction(&Instruction::End);
                self.depth -= 1;
            }

            // OF is only defined for a shift count of 1. For counts > 1, OF is undefined;
            // conservatively leave it unchanged.
            if flags.contains(FlagSet::OF) {
                self.func.instruction(&Instruction::LocalGet(amt_local));
                self.func.instruction(&Instruction::I64Const(1));
                self.func.instruction(&Instruction::I64Eq);
                self.func.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                {
                    match op {
                        BinOp::Shl => self.emit_set_flag(Flag::Of, |this| {
                            // (lhs ^ res) has the sign bit set if MSB(result) != MSB(lhs).
                            this.func.instruction(&Instruction::LocalGet(lhs_local));
                            this.func.instruction(&Instruction::LocalGet(res_local));
                            this.func.instruction(&Instruction::I64Xor);
                            this.func
                                .instruction(&Instruction::I64Const(sign_bit as i64));
                            this.func.instruction(&Instruction::I64And);
                            this.func.instruction(&Instruction::I64Const(0));
                            this.func.instruction(&Instruction::I64Ne);
                        }),
                        BinOp::Shr => self.emit_set_flag(Flag::Of, |this| {
                            // OF = old MSB.
                            this.func.instruction(&Instruction::LocalGet(lhs_local));
                            this.func
                                .instruction(&Instruction::I64Const(sign_bit as i64));
                            this.func.instruction(&Instruction::I64And);
                            this.func.instruction(&Instruction::I64Const(0));
                            this.func.instruction(&Instruction::I64Ne);
                        }),
                        BinOp::Sar => self.emit_set_flag_const(Flag::Of, false),
                        _ => unreachable!(),
                    }
                }
                self.func.instruction(&Instruction::End);
                self.depth -= 1;
            }
        }
        self.func.instruction(&Instruction::End);
        self.depth -= 1;
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
