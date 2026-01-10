use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemArg, MemoryType, Module, TypeSection, ValType,
};

use crate::cpu::{CpuState, Reg};
use crate::interp::JIT_EXIT_SENTINEL;
use crate::ir::{BinOp, CmpOp, IrBlock, IrOp, MemSize, Operand, Place, Temp};

/// Module name for all imports required by the baseline Tier-1 JIT.
pub const IMPORT_MODULE: &str = "env";

/// Imported linear memory (`WebAssembly.Memory`) shared with the main emulator.
pub const IMPORT_MEMORY: &str = "memory";

// Memory helpers. These are expected to implement MMU translation + faults in the runtime.
pub const IMPORT_MEM_READ_U8: &str = "mem_read_u8";
pub const IMPORT_MEM_READ_U16: &str = "mem_read_u16";
pub const IMPORT_MEM_READ_U32: &str = "mem_read_u32";
pub const IMPORT_MEM_READ_U64: &str = "mem_read_u64";
pub const IMPORT_MEM_WRITE_U8: &str = "mem_write_u8";
pub const IMPORT_MEM_WRITE_U16: &str = "mem_write_u16";
pub const IMPORT_MEM_WRITE_U32: &str = "mem_write_u32";
pub const IMPORT_MEM_WRITE_U64: &str = "mem_write_u64";

/// Page-fault helper (chosen over `mmu_translate` for the baseline ABI).
///
/// This is currently unused by the baseline code generator since all loads/stores are routed
/// through `mem_read_*` and `mem_write_*` helpers which are responsible for faulting.
pub const IMPORT_PAGE_FAULT: &str = "page_fault";

/// Bailout helper used to exit back to the runtime on unsupported IR ops or explicit bailout.
pub const IMPORT_JIT_EXIT: &str = "jit_exit";

/// A compiled basic block is exported as a function named `block`.
pub const EXPORT_BLOCK_FN: &str = "block";

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
    _page_fault: u32,
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
        let ty_page_fault = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I64]);
        let ty_jit_exit = types.len();
        types
            .ty()
            .function([ValType::I32, ValType::I64], [ValType::I64]);
        let ty_block = types.len();
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
            _page_fault: next(&mut next_func),
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
            IMPORT_PAGE_FAULT,
            EntityType::Function(ty_page_fault),
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

    fn temp_local_base(self) -> u32 {
        self.next_rip_local() + 1
    }

    fn temp_local(self, Temp(t): Temp) -> u32 {
        self.temp_local_base() + t
    }

    fn total_i64_locals(self) -> u32 {
        Reg::COUNT as u32 + 2 + self.temps
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
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                self.emit_operand(addr);
                match size {
                    MemSize::U8 => {
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_read_u8));
                        self.f.instruction(&Instruction::I64ExtendI32U);
                    }
                    MemSize::U16 => {
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_read_u16));
                        self.f.instruction(&Instruction::I64ExtendI32U);
                    }
                    MemSize::U32 => {
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_read_u32));
                        self.f.instruction(&Instruction::I64ExtendI32U);
                    }
                    MemSize::U64 => {
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_read_u64));
                    }
                }
                self.emit_set_place(dst);
            }
            IrOp::Store { addr, value, size } => {
                self.f
                    .instruction(&Instruction::LocalGet(self.layout.cpu_ptr_local()));
                self.emit_operand(addr);
                self.emit_operand(value);
                match size {
                    MemSize::U8 => {
                        self.f.instruction(&Instruction::I32WrapI64);
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_write_u8));
                    }
                    MemSize::U16 => {
                        self.f.instruction(&Instruction::I32WrapI64);
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_write_u16));
                    }
                    MemSize::U32 => {
                        self.f.instruction(&Instruction::I32WrapI64);
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_write_u32));
                    }
                    MemSize::U64 => {
                        self.f
                            .instruction(&Instruction::Call(self.imported.mem_write_u64));
                    }
                }
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

/// The Tier-1 bailout sentinel (same as `interp::JIT_EXIT_SENTINEL`) but as `i64`.
pub const JIT_EXIT_SENTINEL_I64: i64 = JIT_EXIT_SENTINEL as i64;
