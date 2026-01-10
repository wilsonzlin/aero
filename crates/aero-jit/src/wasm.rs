use crate::cpu::{CpuState, Flag, PendingFlags, Reg};
use crate::ir::{IrBlock, IrOp, MemSize, ValueId, ValueType};

/// Generate a self-contained WASM module for a single basic block.
///
/// The module imports:
/// - `env.memory` (linear memory)
/// - `env.mem_read_u64(cpu_ptr: i32, addr: i64) -> i64`
/// - `env.mem_write_u64(cpu_ptr: i32, addr: i64, val: i64)`
/// - `env.flag_get(cpu_ptr: i32, flag: i32) -> i32`
///
/// And exports:
/// - `block(cpu_ptr: i32) -> i64` (next RIP)
pub fn codegen_wasm(block: &IrBlock) -> Vec<u8> {
    let mut m = ModuleBuilder::new();

    let ty_mem_read_u64 = m.push_type(&[ValType::I32, ValType::I64], &[ValType::I64]);
    let ty_mem_write_u64 = m.push_type(&[ValType::I32, ValType::I64, ValType::I64], &[]);
    let ty_flag_get = m.push_type(&[ValType::I32, ValType::I32], &[ValType::I32]);
    let ty_block = m.push_type(&[ValType::I32], &[ValType::I64]);

    m.import_memory("env", "memory", 1);
    let func_mem_read_u64 = m.import_func("env", "mem_read_u64", ty_mem_read_u64);
    let func_mem_write_u64 = m.import_func("env", "mem_write_u64", ty_mem_write_u64);
    let func_flag_get = m.import_func("env", "flag_get", ty_flag_get);

    let func_block = m.declare_func(ty_block);
    m.export_func("block", func_block);

    let mut f = FunctionBuilder::new(block.value_types.clone());

    let mut next_value_id = 0u32;
    for op in &block.ops {
        let res = op.result_type().map(|_| {
            let id = ValueId(next_value_id);
            next_value_id += 1;
            id
        });
        f.emit_op(
            op,
            res,
            func_mem_read_u64,
            func_mem_write_u64,
            func_flag_get,
        );
    }

    m.define_func(func_block, f.finish());

    m.finish()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValType {
    I32,
    I64,
    V128,
}

impl ValType {
    fn as_byte(self) -> u8 {
        match self {
            ValType::I32 => 0x7F,
            ValType::I64 => 0x7E,
            ValType::V128 => 0x7B,
        }
    }
}

struct ModuleBuilder {
    types: Vec<(Vec<ValType>, Vec<ValType>)>,
    imports: Vec<Import>,
    funcs: Vec<u32>, // type indices for defined funcs
    exports: Vec<Export>,
    codes: Vec<Vec<u8>>,
}

impl ModuleBuilder {
    fn new() -> Self {
        Self {
            types: Vec::new(),
            imports: Vec::new(),
            funcs: Vec::new(),
            exports: Vec::new(),
            codes: Vec::new(),
        }
    }

    fn push_type(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        let idx = self.types.len() as u32;
        self.types.push((params.to_vec(), results.to_vec()));
        idx
    }

    fn import_memory(&mut self, module: &str, name: &str, min_pages: u32) {
        self.imports.push(Import::Memory {
            module: module.to_string(),
            name: name.to_string(),
            min_pages,
        });
    }

    fn import_func(&mut self, module: &str, name: &str, ty: u32) -> u32 {
        let func_idx = self.imported_func_count() as u32;
        self.imports.push(Import::Func {
            module: module.to_string(),
            name: name.to_string(),
            ty,
        });
        func_idx
    }

    fn declare_func(&mut self, ty: u32) -> u32 {
        let idx = (self.imported_func_count() + self.funcs.len()) as u32;
        self.funcs.push(ty);
        idx
    }

    fn define_func(&mut self, func_index: u32, body: Vec<u8>) {
        let first_defined = self.imported_func_count() as u32;
        let local_idx = (func_index - first_defined) as usize;
        if local_idx >= self.codes.len() {
            self.codes.resize_with(local_idx + 1, Vec::new);
        }
        self.codes[local_idx] = body;
    }

    fn export_func(&mut self, name: &str, func_index: u32) {
        self.exports.push(Export {
            name: name.to_string(),
            kind: ExportKind::Func,
            index: func_index,
        });
    }

    fn imported_func_count(&self) -> usize {
        self.imports
            .iter()
            .filter(|i| matches!(i, Import::Func { .. }))
            .count()
    }

    fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x61, 0x73, 0x6D]); // \0asm
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]); // version 1

        // Type section.
        let mut types_payload = Vec::new();
        encode_vec(
            &mut types_payload,
            self.types.iter(),
            |buf, (params, results)| {
                buf.push(0x60); // func type
                encode_vec(buf, params.iter(), |b, t| b.push(t.as_byte()));
                encode_vec(buf, results.iter(), |b, t| b.push(t.as_byte()));
            },
        );
        write_section(&mut out, 1, &types_payload);

        // Import section.
        if !self.imports.is_empty() {
            let mut imports_payload = Vec::new();
            encode_vec(
                &mut imports_payload,
                self.imports.iter(),
                |buf, imp| match imp {
                    Import::Func { module, name, ty } => {
                        encode_name(buf, module);
                        encode_name(buf, name);
                        buf.push(0x00); // func
                        encode_u32(buf, *ty);
                    }
                    Import::Memory {
                        module,
                        name,
                        min_pages,
                    } => {
                        encode_name(buf, module);
                        encode_name(buf, name);
                        buf.push(0x02); // memory
                        buf.push(0x00); // limits: min only
                        encode_u32(buf, *min_pages);
                    }
                },
            );
            write_section(&mut out, 2, &imports_payload);
        }

        // Function section (types for defined funcs).
        if !self.funcs.is_empty() {
            let mut funcs_payload = Vec::new();
            encode_vec(&mut funcs_payload, self.funcs.iter(), |buf, ty| {
                encode_u32(buf, *ty)
            });
            write_section(&mut out, 3, &funcs_payload);
        }

        // Export section.
        if !self.exports.is_empty() {
            let mut exports_payload = Vec::new();
            encode_vec(&mut exports_payload, self.exports.iter(), |buf, ex| {
                encode_name(buf, &ex.name);
                buf.push(ex.kind.as_byte());
                encode_u32(buf, ex.index);
            });
            write_section(&mut out, 7, &exports_payload);
        }

        // Code section.
        if !self.codes.is_empty() {
            let mut code_payload = Vec::new();
            encode_vec(&mut code_payload, self.codes.iter(), |buf, body| {
                encode_u32(buf, body.len() as u32);
                buf.extend_from_slice(body);
            });
            write_section(&mut out, 10, &code_payload);
        }

        out
    }
}

enum Import {
    Func {
        module: String,
        name: String,
        ty: u32,
    },
    Memory {
        module: String,
        name: String,
        min_pages: u32,
    },
}

struct Export {
    name: String,
    kind: ExportKind,
    index: u32,
}

#[derive(Clone, Copy)]
enum ExportKind {
    Func,
}

impl ExportKind {
    fn as_byte(self) -> u8 {
        match self {
            ExportKind::Func => 0x00,
        }
    }
}

struct FunctionBuilder {
    value_types: Vec<ValueType>,
    body: Vec<u8>,
}

impl FunctionBuilder {
    fn new(value_types: Vec<ValueType>) -> Self {
        Self {
            value_types,
            body: Vec::new(),
        }
    }

    fn finish(self) -> Vec<u8> {
        // Local declarations: params are not included.
        let mut locals = Vec::new();
        let mut i = 0usize;
        while i < self.value_types.len() {
            let ty = self.value_types[i];
            let mut run = 1u32;
            while i + (run as usize) < self.value_types.len()
                && self.value_types[i + (run as usize)] == ty
            {
                run += 1;
            }
            locals.push((run, map_valtype(ty)));
            i += run as usize;
        }

        let mut out = Vec::new();
        encode_vec(&mut out, locals.iter(), |buf, (count, vt)| {
            encode_u32(buf, *count);
            buf.push(vt.as_byte());
        });

        out.extend_from_slice(&self.body);
        out.push(0x0B); // end
        out
    }

    fn emit_op(
        &mut self,
        op: &IrOp,
        result: Option<ValueId>,
        mem_read: u32,
        mem_write: u32,
        flag_get: u32,
    ) {
        // ValueId numbering is "dense in results". We assign each result a wasm local index:
        //   local 0: cpu_ptr (param)
        //   local 1..: ValueId 0.. in order
        let result_local = |value: ValueId| 1 + value.0;

        let store_result = |fb: &mut Self| {
            if let Some(v) = result {
                fb.body.push(0x21); // local.set
                encode_u32(&mut fb.body, result_local(v));
            }
        };

        match op {
            IrOp::I32Const(v) => {
                self.body.push(0x41);
                encode_i32(&mut self.body, *v);
                store_result(self);
            }
            IrOp::I64Const(v) => {
                self.body.push(0x42);
                encode_i64(&mut self.body, *v);
                store_result(self);
            }
            IrOp::I32Eqz { value } => {
                self.emit_local_get(*value);
                self.body.push(0x45); // i32.eqz
                store_result(self);
            }
            IrOp::I64Add { lhs, rhs } => {
                self.emit_local_get(*lhs);
                self.emit_local_get(*rhs);
                self.body.push(0x7C); // i64.add
                store_result(self);
            }
            IrOp::I64Sub { lhs, rhs } => {
                self.emit_local_get(*lhs);
                self.emit_local_get(*rhs);
                self.body.push(0x7D); // i64.sub
                store_result(self);
            }
            IrOp::I64ShlImm { value, shift } => {
                self.emit_local_get(*value);
                self.body.push(0x42);
                encode_i64(&mut self.body, *shift as i64);
                self.body.push(0x86); // i64.shl
                store_result(self);
            }
            IrOp::I64And { lhs, rhs } => {
                self.emit_local_get(*lhs);
                self.emit_local_get(*rhs);
                self.body.push(0x83); // i64.and
                store_result(self);
            }
            IrOp::LoadReg64 { reg } => {
                self.emit_cpu_ptr();
                self.body.push(0x29); // i64.load
                encode_u32(&mut self.body, 3); // align=8
                encode_u32(&mut self.body, reg_offset(*reg));
                store_result(self);
            }
            IrOp::StoreReg64 { reg, value } => {
                self.emit_cpu_ptr();
                self.emit_local_get(*value);
                self.body.push(0x37); // i64.store
                encode_u32(&mut self.body, 3); // align=8
                encode_u32(&mut self.body, reg_offset(*reg));
            }
            IrOp::LoadMem { size, addr } => match size {
                MemSize::U64 => {
                    self.emit_cpu_ptr();
                    self.emit_local_get(*addr);
                    self.body.push(0x10); // call
                    encode_u32(&mut self.body, mem_read);
                    store_result(self);
                }
                _ => {
                    // Keep the baseline backend small: only u64 is used by the current x86 subset.
                    // Still emit a trap-like placeholder that returns 0.
                    self.body.push(0x41);
                    encode_i32(&mut self.body, 0);
                    store_result(self);
                }
            },
            IrOp::StoreMem { size, addr, value } => match size {
                MemSize::U64 => {
                    self.emit_cpu_ptr();
                    self.emit_local_get(*addr);
                    self.emit_local_get(*value);
                    self.body.push(0x10); // call
                    encode_u32(&mut self.body, mem_write);
                }
                _ => {}
            },
            IrOp::SetPendingFlags {
                op,
                width_bits,
                lhs,
                rhs,
                result,
            } => {
                // pending_flags.valid = 1
                self.emit_cpu_ptr();
                self.body.push(0x41);
                encode_i32(&mut self.body, 1);
                self.body.push(0x3A); // i32.store8
                encode_u32(&mut self.body, 0);
                encode_u32(&mut self.body, pending_valid_offset());

                // pending_flags.op
                self.emit_cpu_ptr();
                self.body.push(0x41);
                encode_i32(&mut self.body, *op as u8 as i32);
                self.body.push(0x3A); // i32.store8
                encode_u32(&mut self.body, 0);
                encode_u32(&mut self.body, pending_op_offset());

                // pending_flags.width_bits
                self.emit_cpu_ptr();
                self.body.push(0x41);
                encode_i32(&mut self.body, *width_bits as i32);
                self.body.push(0x3A); // i32.store8
                encode_u32(&mut self.body, 0);
                encode_u32(&mut self.body, pending_width_offset());

                // lhs/rhs/result
                self.emit_cpu_ptr();
                self.emit_local_get(*lhs);
                self.body.push(0x37); // i64.store
                encode_u32(&mut self.body, 3);
                encode_u32(&mut self.body, pending_lhs_offset());

                self.emit_cpu_ptr();
                self.emit_local_get(*rhs);
                self.body.push(0x37);
                encode_u32(&mut self.body, 3);
                encode_u32(&mut self.body, pending_rhs_offset());

                self.emit_cpu_ptr();
                self.emit_local_get(*result);
                self.body.push(0x37);
                encode_u32(&mut self.body, 3);
                encode_u32(&mut self.body, pending_result_offset());
            }
            IrOp::GetFlag { flag } => {
                self.emit_cpu_ptr();
                self.body.push(0x41);
                encode_i32(&mut self.body, flag_id(*flag));
                self.body.push(0x10); // call
                encode_u32(&mut self.body, flag_get);
                store_result(self);
            }
            IrOp::SelectI64 {
                cond,
                if_true,
                if_false,
            } => {
                self.emit_local_get(*if_true);
                self.emit_local_get(*if_false);
                self.emit_local_get(*cond);
                self.body.push(0x1B); // select
                store_result(self);
            }
            IrOp::SetHalted => {
                self.emit_cpu_ptr();
                self.body.push(0x41);
                encode_i32(&mut self.body, 1);
                self.body.push(0x3A); // i32.store8
                encode_u32(&mut self.body, 0);
                encode_u32(&mut self.body, halted_offset());
            }
            IrOp::Return { next_rip } => {
                self.emit_local_get(*next_rip);
                self.body.push(0x0F); // return
            }
        }
    }

    fn emit_cpu_ptr(&mut self) {
        self.body.push(0x20); // local.get
        encode_u32(&mut self.body, 0);
    }

    fn emit_local_get(&mut self, value: ValueId) {
        self.body.push(0x20); // local.get
        encode_u32(&mut self.body, 1 + value.0);
    }
}

fn map_valtype(ty: ValueType) -> ValType {
    match ty {
        ValueType::I32 => ValType::I32,
        ValueType::I64 => ValType::I64,
        ValueType::V128 => ValType::V128,
    }
}

fn reg_offset(reg: Reg) -> u32 {
    let base = core::mem::offset_of!(CpuState, regs) as u32;
    base + (reg.as_usize() as u32) * 8
}

fn halted_offset() -> u32 {
    core::mem::offset_of!(CpuState, halted) as u32
}

fn pending_base() -> u32 {
    core::mem::offset_of!(CpuState, pending_flags) as u32
}

fn pending_valid_offset() -> u32 {
    pending_base() + core::mem::offset_of!(PendingFlags, valid) as u32
}
fn pending_op_offset() -> u32 {
    pending_base() + core::mem::offset_of!(PendingFlags, op) as u32
}
fn pending_width_offset() -> u32 {
    pending_base() + core::mem::offset_of!(PendingFlags, width_bits) as u32
}
fn pending_lhs_offset() -> u32 {
    pending_base() + core::mem::offset_of!(PendingFlags, lhs) as u32
}
fn pending_rhs_offset() -> u32 {
    pending_base() + core::mem::offset_of!(PendingFlags, rhs) as u32
}
fn pending_result_offset() -> u32 {
    pending_base() + core::mem::offset_of!(PendingFlags, result) as u32
}

fn flag_id(flag: Flag) -> i32 {
    match flag {
        Flag::Cf => 0,
        Flag::Zf => 1,
        Flag::Sf => 2,
        Flag::Of => 3,
    }
}

fn write_section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    out.push(id);
    encode_u32(out, payload.len() as u32);
    out.extend_from_slice(payload);
}

fn encode_name(out: &mut Vec<u8>, s: &str) {
    encode_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

fn encode_vec<T>(
    out: &mut Vec<u8>,
    items: impl ExactSizeIterator<Item = T>,
    mut f: impl FnMut(&mut Vec<u8>, T),
) {
    encode_u32(out, items.len() as u32);
    for item in items {
        f(out, item);
    }
}

fn encode_u32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn encode_i32(out: &mut Vec<u8>, v: i32) {
    encode_signed_leb(out, v as i64);
}

fn encode_i64(out: &mut Vec<u8>, v: i64) {
    encode_signed_leb(out, v);
}

fn encode_signed_leb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let byte = (v as u8) & 0x7F;
        let sign_bit = (byte & 0x40) != 0;
        v >>= 7;
        let done = (v == 0 && !sign_bit) || (v == -1 && sign_bit);
        if done {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}
