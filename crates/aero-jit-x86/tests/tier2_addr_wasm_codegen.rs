#![cfg(not(target_arch = "wasm32"))]

use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};
use aero_jit_x86::tier2::wasm_codegen::Tier2WasmCodegen;
use aero_types::Gpr;
use wasmparser::{Operator, Parser, Payload};

fn v(idx: u32) -> ValueId {
    ValueId(idx)
}

#[test]
fn addr_with_power_of_two_scale_does_not_emit_i64_mul() {
    // Ensure the Tier2 wasm codegen emits `shl` for `Addr` scales of 2/4/8 instead of an `i64.mul`.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::LoadReg {
                dst: v(1),
                reg: Gpr::Rbx,
            },
            Instr::Addr {
                dst: v(2),
                base: Operand::Value(v(0)),
                index: Operand::Value(v(1)),
                scale: 8,
                disp: 0,
            },
            Instr::StoreReg {
                reg: Gpr::Rcx,
                src: Operand::Value(v(2)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);

    let mut muls = 0u32;
    let mut shls = 0u32;
    for payload in Parser::new(0).parse_all(&wasm) {
        match payload.expect("payload") {
            Payload::CodeSectionEntry(body) => {
                let mut rdr = body.get_operators_reader().expect("operators reader");
                while !rdr.eof() {
                    match rdr.read().expect("op") {
                        Operator::I64Mul => muls += 1,
                        Operator::I64Shl => shls += 1,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    assert_eq!(muls, 0, "unexpected i64.mul in generated wasm");
    assert!(shls > 0, "expected at least one i64.shl in generated wasm");
}

#[test]
fn shift_binops_do_not_emit_redundant_shift_masks() {
    // WebAssembly shift operators already mask their shift counts, so Tier2 doesn't need to
    // explicitly AND the shift amount with 63 before `i64.shl`/`i64.shr_*`.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::BinOp {
                dst: v(1),
                op: aero_jit_x86::tier2::ir::BinOp::Shl,
                lhs: Operand::Value(v(0)),
                // Use a large constant to ensure masking is still required semantically.
                rhs: Operand::Const(130),
                flags: aero_types::FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(1)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);

    let mut const_63 = 0u32;
    let mut shls = 0u32;
    for payload in Parser::new(0).parse_all(&wasm) {
        match payload.expect("payload") {
            Payload::CodeSectionEntry(body) => {
                let mut rdr = body.get_operators_reader().expect("operators reader");
                while !rdr.eof() {
                    match rdr.read().expect("op") {
                        Operator::I64Const { value } if value == 63 => const_63 += 1,
                        Operator::I64Shl => shls += 1,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    assert_eq!(
        const_63, 0,
        "unexpected explicit shift-count masking (i64.const 63) in generated wasm"
    );
    assert!(shls > 0, "expected at least one i64.shl in generated wasm");
}
