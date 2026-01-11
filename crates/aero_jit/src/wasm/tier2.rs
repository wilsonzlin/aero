use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemArg, MemoryType, Module, TypeSection, ValType,
};

use aero_types::{Flag, Gpr, Width};

use crate::abi::{CPU_GPR_OFF, CPU_RFLAGS_OFF, CPU_RIP_OFF, RFLAGS_RESERVED1};
use crate::opt::RegAllocPlan;
use crate::t2_ir::{BinOp, FlagMask, Instr, Operand, TraceIr, TraceKind, ValueId, REG_COUNT};
use crate::wasm::{
    IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE,
};

/// Export name for a compiled Tier-2 trace.
pub const EXPORT_TRACE_FN: &str = "trace";

/// Import that returns the current code page version for self-modifying code guards.
pub const IMPORT_CODE_PAGE_VERSION: &str = "code_page_version";

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
    code_page_version: u32,
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
    /// - export `trace(cpu_ptr: i32) -> i64` (returns `next_rip`)
    /// - import `env.memory`
    /// - import memory helpers described by the `IMPORT_MEM_*` constants
    /// - import `env.code_page_version(page: i64) -> i64`
    ///
    /// The trace spills cached registers + `CpuState.rflags` on every side exit.
    pub fn compile_trace(&self, trace: &TraceIr, plan: &RegAllocPlan) -> Vec<u8> {
        let value_count = max_value_id(trace).max(1);
        let i64_locals = 2 + plan.local_count + value_count; // next_rip + rflags + cached regs + values

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
        let ty_code_page_version = types.len();
        types.ty().function([ValType::I64], [ValType::I64]);
        let ty_trace = types.len();
        types.ty().function([ValType::I32], [ValType::I64]);
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
            code_page_version: next(&mut next_func),
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
            IMPORT_CODE_PAGE_VERSION,
            EntityType::Function(ty_code_page_version),
        );
        module.section(&imports);

        let mut funcs = FunctionSection::new();
        funcs.function(ty_trace);
        module.section(&funcs);

        let mut exports = ExportSection::new();
        // function indices include imported functions. Memory imports do not count.
        exports.export(EXPORT_TRACE_FN, ExportKind::Func, imported.count);
        module.section(&exports);

        let layout = Layout::new(plan, value_count, i64_locals);
        let written_cached_regs = compute_written_cached_regs(trace, plan);

        let mut f = Function::new(vec![(i64_locals, ValType::I64)]);

        // Load cached regs into locals.
        for reg in all_regs() {
            let idx = reg.as_u8() as usize;
            if let Some(local) = plan.local_for_reg[idx] {
                f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
                f.instruction(&Instruction::I64Load(memarg(CPU_GPR_OFF[idx], 3)));
                f.instruction(&Instruction::LocalSet(layout.reg_local(local)));
            }
        }

        // next_rip defaults to current cpu.rip.
        f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        f.instruction(&Instruction::I64Load(memarg(CPU_RIP_OFF, 3)));
        f.instruction(&Instruction::LocalSet(layout.next_rip_local()));

        // Load initial RFLAGS value.
        f.instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        f.instruction(&Instruction::I64Load(memarg(CPU_RFLAGS_OFF, 3)));
        f.instruction(&Instruction::LocalSet(layout.rflags_local()));

        // Single exit block.
        f.instruction(&Instruction::Block(BlockType::Empty));

        let mut emitter = Emitter {
            f: &mut f,
            layout,
            imported,
            depth: 0,
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
                    .instruction(&Instruction::I64Store(memarg(CPU_GPR_OFF[idx], 3)));
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
            .instruction(&Instruction::I64Const(RFLAGS_RESERVED1 as i64));
        emitter.f.instruction(&Instruction::I64Or);
        emitter
            .f
            .instruction(&Instruction::I64Store(memarg(CPU_RFLAGS_OFF, 3)));

        // Store RIP.
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.cpu_ptr_local()));
        emitter
            .f
            .instruction(&Instruction::LocalGet(layout.next_rip_local()));
        emitter
            .f
            .instruction(&Instruction::I64Store(memarg(CPU_RIP_OFF, 3)));

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
    reg_base: u32,
    value_base: u32,
    local_for_reg: [Option<u32>; REG_COUNT],
}

impl Layout {
    fn new(plan: &RegAllocPlan, value_count: u32, i64_locals: u32) -> Self {
        let next_rip_base = 1;
        let rflags_base = next_rip_base + 1;
        let reg_base = rflags_base + 1;
        let value_base = reg_base + plan.local_count;

        assert_eq!(
            value_base + value_count,
            1 + i64_locals,
            "local layout mismatch"
        );

        Self {
            reg_base,
            value_base,
            local_for_reg: plan.local_for_reg,
        }
    }

    fn cpu_ptr_local(self) -> u32 {
        0
    }

    fn next_rip_local(self) -> u32 {
        1
    }

    fn rflags_local(self) -> u32 {
        2
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
                    let idx = reg.as_u8() as usize;
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.f
                        .instruction(&Instruction::I64Load(memarg(CPU_GPR_OFF[idx], 3)));
                }
                self.f
                    .instruction(&Instruction::LocalSet(self.layout.value_local(dst)));
            }
            Instr::StoreReg { reg, src } => {
                if let Some(local) = self.reg_local_for(reg) {
                    self.emit_operand(src);
                    self.f.instruction(&Instruction::LocalSet(local));
                } else {
                    let idx = reg.as_u8() as usize;
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                    self.emit_operand(src);
                    self.f
                        .instruction(&Instruction::I64Store(memarg(CPU_GPR_OFF[idx], 3)));
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
            Instr::GuardCodeVersion {
                page,
                expected,
                exit_rip,
            } => {
                self.f.instruction(&Instruction::I64Const(page as i64));
                self.f
                    .instruction(&Instruction::Call(self.imported.code_page_version));
                self.f.instruction(&Instruction::I64Const(expected as i64));
                self.f.instruction(&Instruction::I64Ne);
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

    fn emit_set_flags(&mut self, mask: FlagMask, values: crate::t2_ir::FlagValues) {
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

        if mask.intersects(FlagMask::CF) {
            update(Flag::Cf, values.cf);
        }
        if mask.intersects(FlagMask::PF) {
            update(Flag::Pf, values.pf);
        }
        if mask.intersects(FlagMask::AF) {
            update(Flag::Af, values.af);
        }
        if mask.intersects(FlagMask::ZF) {
            update(Flag::Zf, values.zf);
        }
        if mask.intersects(FlagMask::SF) {
            update(Flag::Sf, values.sf);
        }
        if mask.intersects(FlagMask::OF) {
            update(Flag::Of, values.of);
        }
    }

    fn emit_binop(&mut self, dst: ValueId, op: BinOp, lhs: Operand, rhs: Operand, flags: FlagMask) {
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
        if flags.intersects(FlagMask::ZF) {
            self.f
                .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
            self.f.instruction(&Instruction::I64Const(0));
            self.f.instruction(&Instruction::I64Eq);
            self.emit_write_flag(Flag::Zf);
        }

        if flags.intersects(FlagMask::SF) {
            self.f
                .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
            self.f.instruction(&Instruction::I64Const(0));
            self.f.instruction(&Instruction::I64LtS);
            self.emit_write_flag(Flag::Sf);
        }

        if flags.intersects(FlagMask::PF) {
            self.emit_parity_even_i32(self.layout.value_local(dst));
            self.emit_write_flag(Flag::Pf);
        }

        match op {
            BinOp::Add => {
                if flags.intersects(FlagMask::CF) {
                    self.f
                        .instruction(&Instruction::LocalGet(self.layout.value_local(dst)));
                    self.emit_operand(lhs);
                    self.f.instruction(&Instruction::I64LtU);
                    self.emit_write_flag(Flag::Cf);
                }
                if flags.intersects(FlagMask::AF) {
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
                if flags.intersects(FlagMask::OF) {
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
                if flags.intersects(FlagMask::CF) {
                    self.emit_operand(lhs);
                    self.emit_operand(rhs);
                    self.f.instruction(&Instruction::I64LtU);
                    self.emit_write_flag(Flag::Cf);
                }
                if flags.intersects(FlagMask::AF) {
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
                if flags.intersects(FlagMask::OF) {
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
                if flags.intersects(FlagMask::CF) {
                    self.f.instruction(&Instruction::I32Const(0));
                    self.emit_write_flag(Flag::Cf);
                }
                if flags.intersects(FlagMask::AF) {
                    self.f.instruction(&Instruction::I32Const(0));
                    self.emit_write_flag(Flag::Af);
                }
                if flags.intersects(FlagMask::OF) {
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
        self.f
            .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
        self.emit_operand(addr);

        match width {
            Width::W8 => {
                self.f
                    .instruction(&Instruction::Call(self.imported.mem_read_u8));
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
    }

    fn emit_store_mem(&mut self, addr: Operand, src: Operand, width: Width) {
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
