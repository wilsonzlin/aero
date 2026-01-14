use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};
use aero_jit_x86::tier2::wasm_codegen::Tier2WasmCodegen;
use aero_types::Gpr;
use wasmparser::Validator;

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
    max.unwrap_or(0)
}

#[test]
fn tier2_valueids_are_compacted_before_codegen() {
    // Deliberately sparse: a single high ValueId.
    let high = ValueId(10_000);
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: high,
                value: 7,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(high),
            },
            Instr::SideExit { exit_rip: 0x1000 },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());

    assert!(
        max_value_id(&trace) < 10,
        "expected ValueIds to be compacted, got max {:?}",
        max_value_id(&trace)
    );

    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    let mut validator = Validator::new();
    validator
        .validate_all(&wasm)
        .expect("generated wasm should validate");
}
