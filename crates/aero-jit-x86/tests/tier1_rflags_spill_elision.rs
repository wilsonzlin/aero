use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
use aero_types::{Cond, FlagSet, Width};

use wasmparser::{Operator, Parser, Payload};

fn wasm_accesses_cpu_rflags(wasm: &[u8]) -> (bool, bool) {
    let mut has_load = false;
    let mut has_store = false;
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CodeSectionEntry(body) = payload.expect("parse wasm") {
            let mut reader = body.get_operators_reader().expect("operators reader");
            while !reader.eof() {
                match reader.read().expect("read operator") {
                    Operator::I64Load { memarg } => {
                        if memarg.offset == u64::from(abi::CPU_RFLAGS_OFF) {
                            has_load = true;
                        }
                    }
                    Operator::I64Store { memarg } => {
                        if memarg.offset == u64::from(abi::CPU_RFLAGS_OFF) {
                            has_store = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    (has_load, has_store)
}

#[test]
fn tier1_readonly_rflags_loads_without_spill() {
    let entry = 0x1000u64;

    // A block that only reads flags (via EvalCond) but does not write them.
    let mut b = IrBuilder::new(entry);
    let cond = b.eval_cond(Cond::E);
    let block = b.finish(IrTerminator::CondJump {
        cond,
        target: entry + 0x10,
        fallthrough: entry + 0x8,
    });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let (has_load, has_store) = wasm_accesses_cpu_rflags(&wasm);
    assert!(has_load, "expected Tier-1 block to load CpuState.rflags");
    assert!(
        !has_store,
        "expected Tier-1 block to avoid spilling CpuState.rflags when read-only"
    );
}

#[test]
fn tier1_writes_rflags_spills_back() {
    let entry = 0x1000u64;

    // Any flag-producing operation should force a spill of RFLAGS.
    let mut b = IrBuilder::new(entry);
    let lhs = b.const_int(Width::W64, 1);
    let rhs = b.const_int(Width::W64, 2);
    b.cmp_flags(Width::W64, lhs, rhs, FlagSet::ALU);
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let (has_load, has_store) = wasm_accesses_cpu_rflags(&wasm);
    assert!(has_load, "expected Tier-1 block to load CpuState.rflags");
    assert!(has_store, "expected Tier-1 block to spill CpuState.rflags");
}
