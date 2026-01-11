use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemArg, MemoryType, Module, TypeSection, ValType,
};

use aero_types::{Flag, FlagSet, Gpr, Width};

use crate::abi;
use crate::abi::{MMU_ACCESS_READ, MMU_ACCESS_WRITE};
use crate::jit_ctx::JitContext;
use crate::opt::RegAllocPlan;
use crate::t2_ir::{BinOp, FlagValues, Instr, Operand, TraceIr, TraceKind, ValueId, REG_COUNT};
use crate::{
    JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK, PAGE_OFFSET_MASK, PAGE_SHIFT,
    TLB_FLAG_IS_RAM, TLB_FLAG_READ, TLB_FLAG_WRITE,
};
use crate::wasm::{
    IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8,
    IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8,
    IMPORT_MEMORY, IMPORT_MODULE, IMPORT_MMU_TRANSLATE,
};

/// Export name for a compiled Tier-2 trace.
pub const EXPORT_TRACE_FN: &str = "trace";

#[derive(Debug, Clone, Copy)]
pub struct Tier2WasmOptions {
    /// Enable the inline direct-mapped JIT TLB + direct guest RAM fast-path for same-page loads
    /// and stores.
    pub inline_tlb: bool,
}

impl Default for Tier2WasmOptions {
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
    count: u32,
}

pub struct Tier2WasmCodegen;

impl Tier2WasmCodegen {
    pub fn new() -> Self {
        Self
    }

    /// Compile a Tier-2 trace into a standalone WASM module.
    ///
    /// ABI:
    /// - export `trace(cpu_ptr: i32, jit_ctx_ptr: i32) -> i64` (returns `next_rip`)
    /// - import `env.memory`
    /// - import memory helpers described by the `IMPORT_MEM_*` constants
    ///
    /// The trace spills cached registers + `CpuState.rflags` on every side exit.
    pub fn compile_trace(&self, trace: &TraceIr, plan: &RegAllocPlan) -> Vec<u8> {
        self.compile_trace_with_options(trace, plan, Tier2WasmOptions::default())
    }

    pub fn compile_trace_with_options(
        &self,
        trace: &TraceIr,
        plan: &RegAllocPlan,
        options: Tier2WasmOptions,
    ) -> Vec<u8> {
        let value_count = max_value_id(trace).max(1);
        let tlb_locals: u32 = if options.inline_tlb { 5 } else { 0 };
        let i64_locals = 2 + tlb_locals + plan.local_count + value_count; // next_rip + rflags + tlb locals + cached regs + values

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
        let ty_trace = types.len();
        types.ty().function([ValType::I32, ValType::I32], [ValType::I64]);
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
        module.section(&imports);

        let mut funcs = FunctionSection::new();
        funcs.function(ty_trace);
        module.section(&funcs);

        let mut exports = ExportSection::new();
        // function indices include imported functions. Memory imports do not count.
        exports.export(EXPORT_TRACE_FN, ExportKind::Func, imported.count);
        module.section(&exports);

        let layout = Layout::new(plan, value_count, i64_locals, options);
        let written_cached_regs = compute_written_cached_regs(trace, plan);

        let mut f = Function::new(vec![(i64_locals, ValType::I64)]);

        // Load cached regs into locals.
        for reg in all_regs() {
            let idx = reg.as_u8() as usize;
            if let Some(local) = plan.local_for_reg[idx] {
                f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
                f.instruction(&Instruction::I64Load(memarg(gpr_offset(reg), 3)));
                f.instruction(&Instruction::LocalSet(layout.reg_local(local)));
            }
        }

        // next_rip defaults to current cpu.rip.
        f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        f.instruction(&Instruction::I64Load(memarg(abi::CPU_RIP_OFF, 3)));
        f.instruction(&Instruction::LocalSet(layout.next_rip_local()));

        // Load initial RFLAGS value.
        f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        f.instruction(&Instruction::I64Load(memarg(abi::CPU_RFLAGS_OFF, 3)));
        f.instruction(&Instruction::LocalSet(layout.rflags_local()));

        if options.inline_tlb {
            // Load JIT metadata (guest RAM base and TLB salt).
            f.instruction(&Instruction::LocalGet(layout.jit_ctx_ptr_local()));
            f.instruction(&Instruction::I64Load(memarg(JitContext::RAM_BASE_OFFSET, 3)));
            f.instruction(&Instruction::LocalSet(layout.ram_base_local()));

            f.instruction(&Instruction::LocalGet(layout.jit_ctx_ptr_local()));
            f.instruction(&Instruction::I64Load(memarg(JitContext::TLB_SALT_OFFSET, 3)));
            f.instruction(&Instruction::LocalSet(layout.tlb_salt_local()));
        }

        // Single exit block.
        f.instruction(&Instruction::Block(BlockType::Empty));

        let mut emitter = Emitter {
            f: &mut f,
            layout,
            imported,
            depth: 0,
            options,
        };

        emitter.emit_instrs(&trace.prologue);

        match trace.kind {
            TraceKind::Loop => {
                emitter.f.instruction(&Instruction::Loop(BlockType::Empty));
                emitter.depth += 1;
                emitter.emit_instrs(&trace.body);
                // Continue looping.
                emitter.f.instruction(&Instruction::Br(0));
                emitter.f.instruction(&Instruction::End);
                emitter.depth -= 1;
            }
            TraceKind::Linear => {
                emitter.emit_instrs(&trace.body);
            }
        }

        emitter.f.instruction(&Instruction::End); // end exit block

        // Spill cached regs (only those that are written by the trace).
        for reg in all_regs() {
            let idx = reg.as_u8() as usize;
            if !written_cached_regs[idx] {
                continue;
            }
            if let Some(local) = plan.local_for_reg[idx] {
                emitter
                    .f
                    .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
                emitter
                    .f
                    .instruction(&Instruction::LocalGet(layout.reg_local(local)));
                emitter
                    .f
                    .instruction(&Instruction::I64Store(memarg(gpr_offset(reg), 3)));
            }
        }

        // Spill RFLAGS (force reserved bit 1).
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.rflags_local()));
        emitter
            .f
            .instruction(&Instruction::I64Const(abi::RFLAGS_RESERVED1 as i64));
        emitter.f.instruction(&Instruction::I64Or);
        emitter
            .f
            .instruction(&Instruction::I64Store(memarg(abi::CPU_RFLAGS_OFF, 3)));

        // Store RIP.
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.next_rip_local()));
        emitter
            .f
            .instruction(&Instruction::I64Store(memarg(abi::CPU_RIP_OFF, 3)));

        // Return next_rip.
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.next_rip_local()));
        emitter.f.instruction(&Instruction::Return);
        emitter.f.instruction(&Instruction::End);

        let mut code = CodeSection::new();
        code.function(&f);
        module.section(&code);

        module.finish()
    }
}

impl Default for Tier2WasmCodegen {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
struct Layout {
    ram_base: Option<u32>,
    tlb_salt: Option<u32>,
    scratch_vaddr: Option<u32>,
    scratch_vpn: Option<u32>,
    scratch_tlb_data: Option<u32>,
    reg_base: u32,
    value_base: u32,
    local_for_reg: [Option<u32>; REG_COUNT],
}

impl Layout {
    fn new(
        plan: &RegAllocPlan,
        value_count: u32,
        i64_locals: u32,
        options: Tier2WasmOptions,
    ) -> Self {
        // Locals are laid out after the two i32 parameters: `(cpu_ptr, jit_ctx_ptr)`.
        let next_rip_base = 2;
        let rflags_base = next_rip_base + 1;
        let mut next = rflags_base + 1;

        let (ram_base, tlb_salt, scratch_vaddr, scratch_vpn, scratch_tlb_data) =
            if options.inline_tlb {
                let ram_base = next;
                next += 1;
                let tlb_salt = next;
                next += 1;
                let scratch_vaddr = next;
                next += 1;
                let scratch_vpn = next;
                next += 1;
                let scratch_tlb_data = next;
                next += 1;
                (
                    Some(ram_base),
                    Some(tlb_salt),
                    Some(scratch_vaddr),
                    Some(scratch_vpn),
                    Some(scratch_tlb_data),
                )
            } else {
                (None, None, None, None, None)
            };

        let reg_base = next;
        next += plan.local_count;
        let value_base = next;
        next += value_count;

        assert_eq!(
            next,
            2 + i64_locals,
            "local layout mismatch"
        );

        Self {
            ram_base,
            tlb_salt,
            scratch_vaddr,
            scratch_vpn,
            scratch_tlb_data,
            reg_base,
            value_base,
            local_for_reg: plan.local_for_reg,
        }
    }

    fn cpu_ptr_local(self) -> u32 {
        0
    }

    fn jit_ctx_ptr_local(self) -> u32 {
        1
    }

    fn next_rip_local(self) -> u32 {
        2
    }

    fn rflags_local(self) -> u32 {
        3
    }

    fn ram_base_local(self) -> u32 {
        self.ram_base.expect("inline TLB disabled")
    }

    fn tlb_salt_local(self) -> u32 {
        self.tlb_salt.expect("inline TLB disabled")
    }

    fn scratch_vaddr_local(self) -> u32 {
        self.scratch_vaddr.expect("inline TLB disabled")
    }

    fn scratch_vpn_local(self) -> u32 {
        self.scratch_vpn.expect("inline TLB disabled")
    }

    fn scratch_tlb_data_local(self) -> u32 {
        self.scratch_tlb_data.expect("inline TLB disabled")
    }

    fn reg_local(self, local: u32) -> u32 {
        self.reg_base + local
    }

    fn value_local(self, ValueId(v): ValueId) -> u32 {
        self.value_base + v
    }
}

struct Emitter<'a> {
    f: &'a mut Function,
    layout: Layout,
    imported: ImportedFuncs,
    /// Current nesting depth inside the exit block.
    depth: u32,
    options: Tier2WasmOptions,
}

impl Emitter<'_> {
    fn emit_instrs(&mut self, instrs: &[Instr]) {
        for inst in instrs {
            self.emit_instr(inst);
        }
    }

    fn emit_instr(&mut self, inst: &Instr) {
        match *inst {
            Instr::Nop => {}
            Instr::Const { dst, value } => {
                self.f.instruction(&Instruction::I64Const(value as i64));
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            }
            Instr::LoadReg { dst, reg } => {
                if let Some(local) = self.reg_local_for(reg) {
                    self.f.instruction(&Instruction::LocalGet(local));
                } else {
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.f
                        .instruction(&Instruction::I64Load(memarg(gpr_offset(reg), 3)));
                }
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            }
            Instr::StoreReg { reg, src } => {
                if let Some(local) = self.reg_local_for(reg) {
                    self.emit_operand(src);
                    self.f.instruction(&Instruction::LocalSet(local));
                } else {
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.emit_operand(src);
                    self.f
                        .instruction(&Instruction::I64Store(memarg(gpr_offset(reg), 3)));
                }
            }
            Instr::LoadFlag { dst, flag } => {
                self.emit_load_flag(flag);
                self.f.instruction(&Instruction::I64ExtendI32U);
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            }
            Instr::SetFlags { mask, values } => {
                self.emit_set_flags(mask, values);
            }
            Instr::BinOp {
                dst,
                op,
                lhs,
                rhs,
                flags,
            } => {
                self.emit_binop(dst, op, lhs, rhs, flags);
            }
            Instr::Addr {
                dst,
                base,
                index,
                scale,
                disp,
            } => {
                self.emit_operand(base);
                self.emit_operand(index);
                self.f.instruction(&Instruction::I64Const(scale as i64));
                self.f.instruction(&Instruction::I64Mul);
                self.f.instruction(&Instruction::I64Add);
                if disp != 0 {
                    self.f.instruction(&Instruction::I64Const(disp));
                    self.f.instruction(&Instruction::I64Add);
                }
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            }
            Instr::LoadMem { dst, addr, width } => {
                self.emit_load_mem(dst, addr, width);
            }
            Instr::StoreMem { addr, src, width } => {
                self.emit_store_mem(addr, src, width);
            }
            Instr::Guard {
                cond,
                expected,
                exit_rip,
            } => {
                self.emit_operand(cond);
                self.f.instruction(&Instruction::I64Const(0));
                self.f.instruction(&Instruction::I64Ne);

                if expected {
                    self.f.instruction(&Instruction::I32Eqz);
                }

                self.f.instruction(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                self.f.instruction(&Instruction::I64Const(exit_rip as i64));
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
                self.f.instruction(&Instruction::Br(self.depth));
                self.f.instruction(&Instruction::End);
                self.depth -= 1;
            }
            Instr::SideExit { exit_rip } => {
                self.f.instruction(&Instruction::I64Const(exit_rip as i64));
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.next_rip_local()));
                self.f.instruction(&Instruction::Br(self.depth));
            }
        }
    }

    fn reg_local_for(&self, reg: Gpr) -> Option<u32> {
        self.layout
            .local_for_reg
            .get(reg.as_u8() as usize)
            .and_then(|v| *v)
            .map(|local| self.layout.reg_local(local))
    }

    fn emit_operand(&mut self, op: Operand) {
        match op {
            Operand::Const(v) => {
                self.f.instruction(&Instruction::I64Const(v as i64));
            }
            Operand::Value(v) => {
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.value_local(v)));
            }
        }
    }

    fn emit_load_flag(&mut self, flag: Flag) {
        let bit = 1u64 << (flag.rflags_bit() as u32);
        self.f
            .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
        self.f.instruction(&Instruction::I64Const(bit as i64));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I64Const(0));
        self.f.instruction(&Instruction::I64Ne);
    }

    fn emit_set_flags(&mut self, mask: FlagSet, values: FlagValues) {
        // Update only requested bits: clear bit and re-insert if value true.
        let mut update = |flag: Flag, val: bool| {
            let bit = 1u64 << (flag.rflags_bit() as u32);
            self.f
                .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
            self.f.instruction(&Instruction::I64Const(!(bit as i64)));
            self.f.instruction(&Instruction::I64And);
            if val {
                self.f.instruction(&Instruction::I64Const(bit as i64));
                self.f.instruction(&Instruction::I64Or);
            }
            self.f
                .instruction(&Instruction::LocalSet(self.layout.rflags_local()));
        };

        if mask.contains(FlagSet::CF) {
            update(Flag::Cf, values.cf);
        }
        if mask.contains(FlagSet::PF) {
            update(Flag::Pf, values.pf);
        }
        if mask.contains(FlagSet::AF) {
            update(Flag::Af, values.af);
        }
        if mask.contains(FlagSet::ZF) {
            update(Flag::Zf, values.zf);
        }
        if mask.contains(FlagSet::SF) {
            update(Flag::Sf, values.sf);
        }
        if mask.contains(FlagSet::OF) {
            update(Flag::Of, values.of);
        }
    }

    fn emit_binop(&mut self, dst: ValueId, op: BinOp, lhs: Operand, rhs: Operand, flags: FlagSet) {
        // Compute result.
        self.emit_operand(lhs);
        self.emit_operand(rhs);
        match op {
            BinOp::Add => {
                self.f.instruction(&Instruction::I64Add);
            }
            BinOp::Sub => {
                self.f.instruction(&Instruction::I64Sub);
            }
            BinOp::Mul => {
                self.f.instruction(&Instruction::I64Mul);
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
            BinOp::Shr => {
                self.f.instruction(&Instruction::I64Const(63));
                self.f.instruction(&Instruction::I64And);
                self.f.instruction(&Instruction::I64ShrU);
            }
            BinOp::Eq => {
                self.f.instruction(&Instruction::I64Eq);
                self.f.instruction(&Instruction::I64ExtendI32U);
            }
            BinOp::LtU => {
                self.f.instruction(&Instruction::I64LtU);
                self.f.instruction(&Instruction::I64ExtendI32U);
            }
        }
        self.f
            .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));

        if flags.is_empty() {
            return;
        }

        // Emit flags from the stored result.
        if flags.contains(FlagSet::ZF) {
            self.f
                .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
            self.f.instruction(&Instruction::I64Const(0));
            self.f.instruction(&Instruction::I64Eq);
            self.emit_write_flag(Flag::Zf);
        }

        if flags.contains(FlagSet::SF) {
            self.f
                .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
            self.f.instruction(&Instruction::I64Const(0));
            self.f.instruction(&Instruction::I64LtS);
            self.emit_write_flag(Flag::Sf);
        }

        if flags.contains(FlagSet::PF) {
            self.emit_parity_even_i32(self.layout.value_local(dst));
            self.emit_write_flag(Flag::Pf);
        }

        match op {
            BinOp::Add => {
                if flags.contains(FlagSet::CF) {
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
                    self.emit_operand(lhs);
                    self.f.instruction(&Instruction::I64LtU);
                    self.emit_write_flag(Flag::Cf);
                }
                if flags.contains(FlagSet::AF) {
                    self.emit_operand(lhs);
                    self.emit_operand(rhs);
                    self.f.instruction(&Instruction::I64Xor); // lhs ^ rhs
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
                    self.f.instruction(&Instruction::I64Xor); // lhs ^ rhs ^ res
                    self.f.instruction(&Instruction::I64Const(0x10));
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I64Const(0));
                    self.f.instruction(&Instruction::I64Ne);
                    self.emit_write_flag(Flag::Af);
                }
                if flags.contains(FlagSet::OF) {
                    self.emit_operand(lhs);
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
                    self.f.instruction(&Instruction::I64Xor); // lhs ^ res
                    self.emit_operand(rhs);
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
                    self.f.instruction(&Instruction::I64Xor); // rhs ^ res
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I64Const(i64::MIN));
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I64Const(0));
                    self.f.instruction(&Instruction::I64Ne);
                    self.emit_write_flag(Flag::Of);
                }
            }
            BinOp::Sub => {
                if flags.contains(FlagSet::CF) {
                    self.emit_operand(lhs);
                    self.emit_operand(rhs);
                    self.f.instruction(&Instruction::I64LtU);
                    self.emit_write_flag(Flag::Cf);
                }
                if flags.contains(FlagSet::AF) {
                    self.emit_operand(lhs);
                    self.emit_operand(rhs);
                    self.f.instruction(&Instruction::I64Xor); // lhs ^ rhs
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
                    self.f.instruction(&Instruction::I64Xor); // lhs ^ rhs ^ res
                    self.f.instruction(&Instruction::I64Const(0x10));
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I64Const(0));
                    self.f.instruction(&Instruction::I64Ne);
                    self.emit_write_flag(Flag::Af);
                }
                if flags.contains(FlagSet::OF) {
                    self.emit_operand(lhs);
                    self.emit_operand(rhs);
                    self.f.instruction(&Instruction::I64Xor); // lhs ^ rhs
                    self.emit_operand(lhs);
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
                    self.f.instruction(&Instruction::I64Xor); // lhs ^ res
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I64Const(i64::MIN));
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I64Const(0));
                    self.f.instruction(&Instruction::I64Ne);
                    self.emit_write_flag(Flag::Of);
                }
            }
            _ => {
                if flags.contains(FlagSet::CF) {
                    self.f.instruction(&Instruction::I32Const(0));
                    self.emit_write_flag(Flag::Cf);
                }
                if flags.contains(FlagSet::AF) {
                    self.f.instruction(&Instruction::I32Const(0));
                    self.emit_write_flag(Flag::Af);
                }
                if flags.contains(FlagSet::OF) {
                    self.f.instruction(&Instruction::I32Const(0));
                    self.emit_write_flag(Flag::Of);
                }
            }
        }
    }

    fn emit_write_flag(&mut self, flag: Flag) {
        let bit = 1u64 << (flag.rflags_bit() as u32);
        // Stack: i32 flag_value
        // rflags = (rflags & !bit) | (flag_value ? bit : 0)
        self.f
            .instruction(&Instruction::If(BlockType::Result(ValType::I64)));
        self.f.instruction(&Instruction::I64Const(bit as i64));
        self.f.instruction(&Instruction::Else);
        self.f.instruction(&Instruction::I64Const(0));
        self.f.instruction(&Instruction::End); // produces i64

        self.f
            .instruction(&Instruction::LocalGet(self.layout.rflags_local()));
        self.f.instruction(&Instruction::I64Const(!(bit as i64)));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I64Or);
        self.f
            .instruction(&Instruction::LocalSet(self.layout.rflags_local()));
    }

    fn emit_parity_even_i32(&mut self, res_local: u32) {
        self.f.instruction(&Instruction::LocalGet(res_local));
        self.f.instruction(&Instruction::I64Const(0xff));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I32WrapI64);
        self.f.instruction(&Instruction::I32Popcnt);
        self.f.instruction(&Instruction::I32Const(1));
        self.f.instruction(&Instruction::I32And);
        self.f.instruction(&Instruction::I32Eqz);
    }

    fn emit_load_mem(&mut self, dst: ValueId, addr: Operand, width: Width) {
        if !self.options.inline_tlb {
            self.f
                .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
            self.emit_operand(addr);

            match width {
                Width::W8 => {
                    self.f.instruction(&Instruction::Call(self.imported.mem_read_u8));
                    self.f.instruction(&Instruction::I64ExtendI32U);
                }
                Width::W16 => {
                    self.f
                        .instruction(&Instruction::Call(self.imported.mem_read_u16));
                    self.f.instruction(&Instruction::I64ExtendI32U);
                }
                Width::W32 => {
                    self.f
                        .instruction(&Instruction::Call(self.imported.mem_read_u32));
                    self.f.instruction(&Instruction::I64ExtendI32U);
                }
                Width::W64 => {
                    self.f
                        .instruction(&Instruction::Call(self.imported.mem_read_u64));
                }
            }

            self.f
                .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            return;
        }

        // Save vaddr into a scratch local (used by both slow/fast paths).
        self.emit_operand(addr);
        self.f
            .instruction(&Instruction::LocalSet(self.layout.scratch_vaddr_local()));

        let (size_bytes, slow_read) = match width {
            Width::W8 => (1u32, self.imported.mem_read_u8),
            Width::W16 => (2u32, self.imported.mem_read_u16),
            Width::W32 => (4u32, self.imported.mem_read_u32),
            Width::W64 => (8u32, self.imported.mem_read_u64),
        };

        // Cross-page accesses use the slow helper for correctness.
        let cross_limit =
            (PAGE_OFFSET_MASK as u64).saturating_sub(size_bytes.saturating_sub(1) as u64);
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.f
            .instruction(&Instruction::I64Const(PAGE_OFFSET_MASK as i64));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I64Const(cross_limit as i64));
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
            if !matches!(width, Width::W64) {
                self.f.instruction(&Instruction::I64ExtendI32U);
            }
            self.f
                .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
        }
        self.f.instruction(&Instruction::Else);
        {
            // Fast path: inline TLB probe + direct RAM load.
            self.emit_translate_and_cache(MMU_ACCESS_READ, TLB_FLAG_READ);

            // If the translation resolves to MMIO/ROM/unmapped, fall back to the slow helper.
            self.f
                .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
            self.f
                .instruction(&Instruction::I64Const(TLB_FLAG_IS_RAM as i64));
            self.f.instruction(&Instruction::I64And);
            self.f.instruction(&Instruction::I64Eqz);

            self.f
                .instruction(&Instruction::If(BlockType::Empty));
            self.depth += 1;
            {
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                self.f.instruction(&Instruction::Call(slow_read));
                if !matches!(width, Width::W64) {
                    self.f.instruction(&Instruction::I64ExtendI32U);
                }
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            }
            self.f.instruction(&Instruction::Else);
            {
                self.emit_compute_ram_addr();
                match width {
                    Width::W8 => self.f.instruction(&Instruction::I64Load8U(memarg(0, 0))),
                    Width::W16 => self.f.instruction(&Instruction::I64Load16U(memarg(0, 1))),
                    Width::W32 => self.f.instruction(&Instruction::I64Load32U(memarg(0, 2))),
                    Width::W64 => self.f.instruction(&Instruction::I64Load(memarg(0, 3))),
                };
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            }
            self.f.instruction(&Instruction::End);
            self.depth -= 1;
        }
        self.f.instruction(&Instruction::End);
        self.depth -= 1;
    }

    fn emit_store_mem(&mut self, addr: Operand, src: Operand, width: Width) {
        if !self.options.inline_tlb {
            self.f
                .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
            self.emit_operand(addr);
            self.emit_operand(src);

            match width {
                Width::W8 => {
                    self.f.instruction(&Instruction::I64Const(0xff));
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I32WrapI64);
                    self.f
                        .instruction(&Instruction::Call(self.imported.mem_write_u8));
                }
                Width::W16 => {
                    self.f.instruction(&Instruction::I64Const(0xffff));
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I32WrapI64);
                    self.f
                        .instruction(&Instruction::Call(self.imported.mem_write_u16));
                }
                Width::W32 => {
                    self.f
                        .instruction(&Instruction::I64Const(0xffff_ffffu64 as i64));
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I32WrapI64);
                    self.f
                        .instruction(&Instruction::Call(self.imported.mem_write_u32));
                }
                Width::W64 => {
                    self.f
                        .instruction(&Instruction::Call(self.imported.mem_write_u64));
                }
            }
            return;
        }

        self.emit_operand(addr);
        self.f
            .instruction(&Instruction::LocalSet(self.layout.scratch_vaddr_local()));

        let (size_bytes, slow_write) = match width {
            Width::W8 => (1u32, self.imported.mem_write_u8),
            Width::W16 => (2u32, self.imported.mem_write_u16),
            Width::W32 => (4u32, self.imported.mem_write_u32),
            Width::W64 => (8u32, self.imported.mem_write_u64),
        };

        let cross_limit =
            (PAGE_OFFSET_MASK as u64).saturating_sub(size_bytes.saturating_sub(1) as u64);
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.f
            .instruction(&Instruction::I64Const(PAGE_OFFSET_MASK as i64));
        self.f.instruction(&Instruction::I64And);
        self.f.instruction(&Instruction::I64Const(cross_limit as i64));
        self.f.instruction(&Instruction::I64GtU);

        self.f.instruction(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        {
            // Slow path.
            self.f
                .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
            self.f
                .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
            self.emit_operand(src);
            if !matches!(width, Width::W64) {
                match width {
                    Width::W8 => {
                        self.f.instruction(&Instruction::I64Const(0xff));
                    }
                    Width::W16 => {
                        self.f.instruction(&Instruction::I64Const(0xffff));
                    }
                    Width::W32 => {
                        self.f
                            .instruction(&Instruction::I64Const(0xffff_ffffu64 as i64));
                    }
                    Width::W64 => unreachable!("masking only required for <= 32-bit stores"),
                };
                self.f.instruction(&Instruction::I64And);
                self.f.instruction(&Instruction::I32WrapI64);
            }
            self.f.instruction(&Instruction::Call(slow_write));
        }
        self.f.instruction(&Instruction::Else);
        {
            // Fast path: inline TLB probe + direct RAM store.
            self.emit_translate_and_cache(MMU_ACCESS_WRITE, TLB_FLAG_WRITE);

            self.f
                .instruction(&Instruction::LocalGet(self.layout.scratch_tlb_data_local()));
            self.f
                .instruction(&Instruction::I64Const(TLB_FLAG_IS_RAM as i64));
            self.f.instruction(&Instruction::I64And);
            self.f.instruction(&Instruction::I64Eqz);

            self.f
                .instruction(&Instruction::If(BlockType::Empty));
            self.depth += 1;
            {
                // MMIO/ROM/unmapped: fall back to the slow helper.
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
                self.emit_operand(src);
                if !matches!(width, Width::W64) {
                    match width {
                        Width::W8 => {
                            self.f.instruction(&Instruction::I64Const(0xff));
                        }
                        Width::W16 => {
                            self.f.instruction(&Instruction::I64Const(0xffff));
                        }
                        Width::W32 => {
                            self.f
                                .instruction(&Instruction::I64Const(0xffff_ffffu64 as i64));
                        }
                        Width::W64 => unreachable!("masking only required for <= 32-bit stores"),
                    };
                    self.f.instruction(&Instruction::I64And);
                    self.f.instruction(&Instruction::I32WrapI64);
                }
                self.f.instruction(&Instruction::Call(slow_write));
            }
            self.f.instruction(&Instruction::Else);
            {
                self.emit_compute_ram_addr();
                self.emit_operand(src);
                match width {
                    Width::W8 => self.f.instruction(&Instruction::I64Store8(memarg(0, 0))),
                    Width::W16 => self.f.instruction(&Instruction::I64Store16(memarg(0, 1))),
                    Width::W32 => self.f.instruction(&Instruction::I64Store32(memarg(0, 2))),
                    Width::W64 => self.f.instruction(&Instruction::I64Store(memarg(0, 3))),
                };
            }
            self.f.instruction(&Instruction::End);
            self.depth -= 1;
        }
        self.f.instruction(&Instruction::End);
        self.depth -= 1;
    }

    fn emit_translate_and_cache(&mut self, access_code: i32, required_flag: u64) {
        debug_assert!(self.options.inline_tlb);

        // vpn = vaddr >> 12
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.f.instruction(&Instruction::I64Const(PAGE_SHIFT as i64));
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
            // Miss: call the translation helper (expected to fill the entry).
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
            .instruction(&Instruction::LocalGet(self.layout.jit_ctx_ptr_local()));
        self.f
            .instruction(&Instruction::LocalGet(self.layout.scratch_vaddr_local()));
        self.f.instruction(&Instruction::I32Const(access_code));
        self.f.instruction(&Instruction::Call(
            self.imported
                .mmu_translate
                .expect("mmu_translate import missing"),
        ));
        self.f
            .instruction(&Instruction::LocalSet(self.layout.scratch_tlb_data_local()));
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
        // base = jit_ctx_ptr + JitContext::TLB_OFFSET + ((vpn & mask) * ENTRY_SIZE)
        self.f
            .instruction(&Instruction::LocalGet(self.layout.jit_ctx_ptr_local()));
        self.f.instruction(&Instruction::I64ExtendI32U);
        self.f
            .instruction(&Instruction::I64Const(JitContext::TLB_OFFSET as i64));
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
}

fn memarg(offset: u32, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}

fn next(idx: &mut u32) -> u32 {
    let cur = *idx;
    *idx += 1;
    cur
}

fn max_value_id(trace: &TraceIr) -> u32 {
    let mut max: Option<u32> = None;
    for inst in trace.iter_instrs() {
        if let Some(dst) = inst.dst() {
            max = Some(max.map_or(dst.0, |cur| cur.max(dst.0)));
        }
        inst.for_each_operand(|op| {
            if let Operand::Value(v) = op {
                max = Some(max.map_or(v.0, |cur| cur.max(v.0)));
            }
        });
    }
    max.map_or(0, |v| v + 1)
}

fn compute_written_cached_regs(trace: &TraceIr, plan: &RegAllocPlan) -> [bool; REG_COUNT] {
    let mut written = [false; REG_COUNT];
    for inst in trace.iter_instrs() {
        if let Instr::StoreReg { reg, .. } = *inst {
            let idx = reg.as_u8() as usize;
            if plan.local_for_reg[idx].is_some() {
                written[idx] = true;
            }
        }
    }
    written
}

fn all_regs() -> [Gpr; REG_COUNT] {
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

fn gpr_offset(reg: Gpr) -> u32 {
    crate::abi::CPU_GPR_OFF[reg.as_u8() as usize]
}
