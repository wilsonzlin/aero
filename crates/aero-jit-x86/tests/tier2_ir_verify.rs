use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::verify::verify_trace;
use aero_types::Gpr;

#[test]
fn tier2_verify_rejects_use_of_undefined_value() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![Instr::StoreReg {
            reg: Gpr::Rax,
            src: Operand::Value(ValueId(1)),
        }],
        kind: TraceKind::Linear,
    };

    assert!(verify_trace(&trace).is_err());
}
