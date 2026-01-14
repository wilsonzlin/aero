use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::wasm_codegen::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_types::Width;
use wasmparser::{Operator, Parser, Payload, Validator};

fn assert_wasm_contains_op(wasm: &[u8], predicate: impl Fn(&Operator<'_>) -> bool) {
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.expect("parse wasm section");
        if let Payload::CodeSectionEntry(body) = payload {
            let mut reader = body.get_operators_reader().expect("operators reader");
            while !reader.eof() {
                let op = reader.read().expect("operator");
                if predicate(&op) {
                    return;
                }
            }
        }
    }
    panic!("did not find expected wasm operator");
}

#[test]
fn tier2_code_version_table_ops_use_atomics_for_shared_memory() {
    // Build a trace that:
    // - emits an inline code-version guard (code_version_guard_import=false)
    // - emits an inline-TLB store fast-path, which bumps the code-version table
    let trace = TraceIr {
        prologue: vec![Instr::GuardCodeVersion {
            page: 0,
            expected: 123,
            exit_rip: 0x9999,
        }],
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x100),
            src: Operand::Const(0xab),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            inline_tlb: true,
            code_version_guard_import: false,
            memory_shared: true,
            ..Default::default()
        },
    );

    Validator::new()
        .validate_all(&wasm)
        .expect("generated WASM should validate");

    // Inline guard should use `i32.atomic.load`.
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::I32AtomicLoad { .. }));

    // Store fast-path should bump the version table with `i32.atomic.rmw.add`.
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::I32AtomicRmwAdd { .. }));
}
