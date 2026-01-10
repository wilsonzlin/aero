use crate::simd::sse::{Inst, Operand, Program, XmmReg};
use crate::simd::state::{MXCSR_DEFAULT, XMM_BYTES, XMM_REG_COUNT};
use thiserror::Error;
use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction,
    MemArg, MemorySection, MemoryType, Module, TypeSection, ValType,
};

#[derive(Clone, Copy, Debug)]
pub struct WasmLayout {
    /// Base address (within the generated wasm linear memory) of the serialized `SseState`.
    pub state_base: u32,
    /// Base address (within the generated wasm linear memory) of guest memory.
    pub guest_mem_base: u32,
}

pub const DEFAULT_WASM_LAYOUT: WasmLayout = WasmLayout {
    state_base: 0,
    guest_mem_base: 1024,
};

#[derive(Clone, Copy, Debug)]
pub struct JitOptions {
    /// Enable lowering `PSHUFB` via `i8x16.swizzle` (requires SSSE3 on the guest).
    pub enable_ssse3: bool,
}

impl Default for JitOptions {
    fn default() -> Self {
        Self { enable_ssse3: true }
    }
}

pub fn compile_wasm_simd(
    program: &Program,
    options: JitOptions,
    layout: WasmLayout,
) -> Result<Vec<u8>, JitError> {
    for inst in &program.insts {
        if matches!(inst, Inst::Pshufb { .. }) && !options.enable_ssse3 {
            return Err(JitError::RequiresSsse3);
        }
    }

    let needs_default_mxcsr = program.insts.iter().any(|inst| {
        matches!(
            inst,
            Inst::Addps { .. }
                | Inst::Subps { .. }
                | Inst::Mulps { .. }
                | Inst::Addpd { .. }
                | Inst::Subpd { .. }
                | Inst::Mulpd { .. }
        )
    });

    // We always declare three scratch v128 locals. This keeps the instruction emission simple
    // while still producing SIMD opcodes in the output.
    let mut func = Function::new(vec![(3, ValType::V128)]);

    if needs_default_mxcsr {
        emit_mxcsr_check(&mut func, layout)?;
    }

    for inst in &program.insts {
        match *inst {
            Inst::MovdquLoad { dst, addr } => {
                emit_load_mem_to_local(&mut func, addr, layout, Local::Tmp0)?;
                emit_store_local_to_reg(&mut func, dst, layout, Local::Tmp0)?;
            }
            Inst::MovdquStore { addr, src } => {
                emit_load_reg_to_local(&mut func, src, layout, Local::Tmp0)?;
                emit_store_local_to_mem(&mut func, addr, layout, Local::Tmp0)?;
            }

            Inst::Addps { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::F32x4Add)?,
            Inst::Subps { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::F32x4Sub)?,
            Inst::Mulps { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::F32x4Mul)?,

            Inst::Addpd { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::F64x2Add)?,
            Inst::Subpd { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::F64x2Sub)?,
            Inst::Mulpd { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::F64x2Mul)?,

            Inst::Pand { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::V128And)?,
            Inst::Por { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::V128Or)?,
            Inst::Pxor { dst, src } => emit_binop(&mut func, dst, src, layout, Instruction::V128Xor)?,

            Inst::Pshufb { dst, src } => emit_pshufb(&mut func, dst, src, layout)?,
        }
    }

    func.instruction(&Instruction::End);

    let mut module = Module::new();

    let mut types = TypeSection::new();
    let run_ty = types.len();
    types.ty().function([], []);
    module.section(&types);

    let mut funcs = FunctionSection::new();
    funcs.function(run_ty);
    module.section(&funcs);

    let mut mems = MemorySection::new();
    // Single-page memory is sufficient for unit tests and demo programs.
    mems.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&mems);

    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, 0);
    exports.export("mem", ExportKind::Memory, 0);
    module.section(&exports);

    let mut code = CodeSection::new();
    code.function(&func);
    module.section(&code);

    Ok(module.finish())
}

fn emit_mxcsr_check(func: &mut Function, layout: WasmLayout) -> Result<(), JitError> {
    let mxcsr_off = (XMM_REG_COUNT * XMM_BYTES) as u32;
    let addr = layout
        .state_base
        .checked_add(mxcsr_off)
        .ok_or(JitError::AddressOverflow)?;

    func.instruction(&Instruction::I32Const(addr as i32));
    func.instruction(&Instruction::I32Load(memarg_i32()));
    func.instruction(&Instruction::I32Const(MXCSR_DEFAULT as i32));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);
    Ok(())
}

#[derive(Clone, Copy)]
enum Local {
    Tmp0 = 0,
    Tmp1 = 1,
    Tmp2 = 2,
}

fn emit_binop(
    func: &mut Function,
    dst: XmmReg,
    src: Operand,
    layout: WasmLayout,
    op: Instruction<'static>,
) -> Result<(), JitError> {
    emit_load_reg(dst, func, layout)?;
    emit_load_operand(src, func, layout)?;
    func.instruction(&op);
    func.instruction(&Instruction::LocalSet(Local::Tmp0 as u32));
    emit_store_local_to_reg(func, dst, layout, Local::Tmp0)?;
    Ok(())
}

fn emit_pshufb(func: &mut Function, dst: XmmReg, src: Operand, layout: WasmLayout) -> Result<(), JitError> {
    // tmp0 = data (dst)
    emit_load_reg(dst, func, layout)?;
    func.instruction(&Instruction::LocalSet(Local::Tmp0 as u32));

    // tmp1 = control (src)
    emit_load_operand(src, func, layout)?;
    func.instruction(&Instruction::LocalSet(Local::Tmp1 as u32));

    // tmp2 = masked indices (control & 0x0F)
    func.instruction(&Instruction::LocalGet(Local::Tmp1 as u32));
    func.instruction(&Instruction::V128Const(v128_const_splat_u8(0x0F)));
    func.instruction(&Instruction::V128And);
    func.instruction(&Instruction::LocalSet(Local::Tmp2 as u32));

    // tmp1 = highbit mask: (control & 0x80) != 0
    func.instruction(&Instruction::LocalGet(Local::Tmp1 as u32));
    func.instruction(&Instruction::V128Const(v128_const_splat_u8(0x80)));
    func.instruction(&Instruction::V128And);
    func.instruction(&Instruction::V128Const(0));
    func.instruction(&Instruction::I8x16Ne);
    func.instruction(&Instruction::LocalSet(Local::Tmp1 as u32));

    // tmp2 = indices with OOB (16) where highbit set
    func.instruction(&Instruction::V128Const(v128_const_splat_u8(0x10)));
    func.instruction(&Instruction::LocalGet(Local::Tmp2 as u32));
    func.instruction(&Instruction::LocalGet(Local::Tmp1 as u32));
    func.instruction(&Instruction::V128Bitselect);
    func.instruction(&Instruction::LocalSet(Local::Tmp2 as u32));

    // tmp0 = swizzle(data, indices)
    func.instruction(&Instruction::LocalGet(Local::Tmp0 as u32));
    func.instruction(&Instruction::LocalGet(Local::Tmp2 as u32));
    func.instruction(&Instruction::I8x16Swizzle);
    func.instruction(&Instruction::LocalSet(Local::Tmp0 as u32));

    emit_store_local_to_reg(func, dst, layout, Local::Tmp0)?;
    Ok(())
}

fn emit_load_operand(op: Operand, func: &mut Function, layout: WasmLayout) -> Result<(), JitError> {
    match op {
        Operand::Reg(reg) => emit_load_reg(reg, func, layout),
        Operand::Mem(addr) => emit_load_mem(addr, func, layout),
    }
}

fn emit_load_reg(reg: XmmReg, func: &mut Function, layout: WasmLayout) -> Result<(), JitError> {
    let addr = layout
        .state_base
        .checked_add(reg.state_byte_offset())
        .ok_or(JitError::AddressOverflow)?;
    func.instruction(&Instruction::I32Const(addr as i32));
    func.instruction(&Instruction::V128Load(memarg_v128()));
    Ok(())
}

fn emit_load_mem(addr: u32, func: &mut Function, layout: WasmLayout) -> Result<(), JitError> {
    let addr = layout
        .guest_mem_base
        .checked_add(addr)
        .ok_or(JitError::AddressOverflow)?;
    func.instruction(&Instruction::I32Const(addr as i32));
    func.instruction(&Instruction::V128Load(memarg_v128()));
    Ok(())
}

fn emit_load_reg_to_local(
    func: &mut Function,
    reg: XmmReg,
    layout: WasmLayout,
    local: Local,
) -> Result<(), JitError> {
    emit_load_reg(reg, func, layout)?;
    func.instruction(&Instruction::LocalSet(local as u32));
    Ok(())
}

fn emit_store_local_to_reg(
    func: &mut Function,
    reg: XmmReg,
    layout: WasmLayout,
    local: Local,
) -> Result<(), JitError> {
    let addr = layout
        .state_base
        .checked_add(reg.state_byte_offset())
        .ok_or(JitError::AddressOverflow)?;
    func.instruction(&Instruction::I32Const(addr as i32));
    func.instruction(&Instruction::LocalGet(local as u32));
    func.instruction(&Instruction::V128Store(memarg_v128()));
    Ok(())
}

fn emit_load_mem_to_local(
    func: &mut Function,
    addr: u32,
    layout: WasmLayout,
    local: Local,
) -> Result<(), JitError> {
    emit_load_mem(addr, func, layout)?;
    func.instruction(&Instruction::LocalSet(local as u32));
    Ok(())
}

fn emit_store_local_to_mem(
    func: &mut Function,
    addr: u32,
    layout: WasmLayout,
    local: Local,
) -> Result<(), JitError> {
    let addr = layout
        .guest_mem_base
        .checked_add(addr)
        .ok_or(JitError::AddressOverflow)?;
    func.instruction(&Instruction::I32Const(addr as i32));
    func.instruction(&Instruction::LocalGet(local as u32));
    func.instruction(&Instruction::V128Store(memarg_v128()));
    Ok(())
}

fn v128_const_splat_u8(v: u8) -> i128 {
    i128::from_le_bytes([v; 16])
}

fn memarg_v128() -> MemArg {
    MemArg {
        offset: 0,
        align: 4, // 2^4 = 16-byte alignment
        memory_index: 0,
    }
}

fn memarg_i32() -> MemArg {
    MemArg {
        offset: 0,
        align: 2, // 2^2 = 4-byte alignment
        memory_index: 0,
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum JitError {
    #[error("requires SSSE3 (PSHUFB) but JIT options have SSSE3 disabled")]
    RequiresSsse3,

    #[error("address arithmetic overflow while lowering to wasm")]
    AddressOverflow,
}

