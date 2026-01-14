use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_types::Gpr;

#[test]
fn tier2_trace_ir_validate_rejects_out_of_range_value_use() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![Instr::StoreReg {
            reg: Gpr::Rax,
            src: Operand::Value(ValueId(0)),
        }],
        kind: TraceKind::Linear,
    };

    let err = trace.validate().unwrap_err();
    assert!(
        err.contains("exceeds max_value_id"),
        "unexpected error message: {err}"
    );
}

#[test]
fn tier2_trace_ir_validate_rejects_use_before_def() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(1)),
            },
            Instr::Const {
                dst: ValueId(1),
                value: 123,
            },
        ],
        kind: TraceKind::Linear,
    };

    let err = trace.validate().unwrap_err();
    assert!(
        err.contains("use-before-def"),
        "unexpected error message: {err}"
    );
}

#[test]
fn tier2_trace_ir_validate_rejects_duplicate_value_defs() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: 1,
            },
            Instr::Const {
                dst: ValueId(0),
                value: 2,
            },
        ],
        kind: TraceKind::Linear,
    };

    let err = trace.validate().unwrap_err();
    assert!(
        err.contains("defined multiple times"),
        "unexpected error message: {err}"
    );
}

#[test]
fn tier2_trace_ir_validate_rejects_side_exit_not_last() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![Instr::SideExit { exit_rip: 0x1234 }, Instr::Nop],
        kind: TraceKind::Linear,
    };

    let err = trace.validate().unwrap_err();
    assert!(
        err.contains("final instruction"),
        "unexpected error message: {err}"
    );
}

#[test]
fn tier2_trace_ir_validate_rejects_empty_loop_body() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![],
        kind: TraceKind::Loop,
    };

    let err = trace.validate().unwrap_err();
    assert!(
        err.contains("non-empty body"),
        "unexpected error message: {err}"
    );
}
