use aero_jit_x86::tier2::ir::{Instr, TraceIr, TraceKind};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::{Tier2WasmCodegen, Tier2WasmOptions};
use wasmparser::{Operator, Parser, Payload};

#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions};
#[cfg(feature = "tier1-inline-tlb")]
use aero_types::Width;

fn wasm_contains_operator<F>(wasm: &[u8], mut f: F) -> bool
where
    F: FnMut(&Operator<'_>) -> bool,
{
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CodeSectionEntry(body) = payload.expect("parse wasm") {
            let mut ops = body.get_operators_reader().expect("get operators reader");
            while !ops.eof() {
                let op = ops.read().expect("read operator");
                if f(&op) {
                    return true;
                }
            }
        }
    }
    false
}

fn wasm_count_operator<F>(wasm: &[u8], mut f: F) -> usize
where
    F: FnMut(&Operator<'_>) -> bool,
{
    let mut count = 0usize;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.expect("parse wasm") {
            Payload::CodeSectionEntry(body) => {
                let mut ops = body.get_operators_reader().expect("get operators reader");
                while !ops.eof() {
                    let op = ops.read().expect("read operator");
                    if f(&op) {
                        count += 1;
                    }
                }
            }
            _ => {}
        }
    }
    count
}

#[cfg(feature = "tier1-inline-tlb")]
#[test]
fn tier1_shared_memory_inline_tlb_store_uses_atomic_rmw_add_for_code_version_bumps() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0x1000);
    let value = b.const_int(Width::W64, 0x1234);
    b.store(Width::W64, addr, value);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &ir,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_stores: true,
            memory_shared: true,
            ..Default::default()
        },
    );

    wasmparser::Validator::new()
        .validate_all(&wasm)
        .expect("generated WASM should validate");

    assert!(
        wasm_contains_operator(&wasm, |op| matches!(op, Operator::I32AtomicRmwAdd { .. })),
        "expected Tier-1 inline store code-version bump to use i32.atomic.rmw.add when memory is shared"
    );
}

#[cfg(feature = "tier1-inline-tlb")]
#[test]
fn tier1_shared_memory_cross_page_store_uses_atomic_rmw_add_for_both_page_bumps() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, aero_jit_x86::PAGE_SIZE - 1);
    let value = b.const_int(Width::W64, 0x1234);
    b.store(Width::W64, addr, value);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &ir,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_stores: true,
            inline_tlb_cross_page_fastpath: true,
            memory_shared: true,
            ..Default::default()
        },
    );

    wasmparser::Validator::new()
        .validate_all(&wasm)
        .expect("generated WASM should validate");

    let rmw_adds = wasm_count_operator(&wasm, |op| matches!(op, Operator::I32AtomicRmwAdd { .. }));
    assert!(
        rmw_adds >= 2,
        "expected Tier-1 cross-page inline store to bump both pages with i32.atomic.rmw.add when memory is shared (found {rmw_adds})"
    );
}

#[test]
fn tier2_shared_memory_inline_code_version_guard_uses_atomic_load() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::GuardCodeVersion {
                page: 0,
                expected: 0,
                exit_rip: 0x3000,
            },
            Instr::SideExit { exit_rip: 0x4000 },
        ],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();

    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            memory_shared: true,
            code_version_guard_import: false,
            ..Default::default()
        },
    );

    wasmparser::Validator::new()
        .validate_all(&wasm)
        .expect("generated WASM should validate");

    assert!(
        wasm_contains_operator(&wasm, |op| matches!(op, Operator::I32AtomicLoad { .. })),
        "expected Tier-2 inline code-version guard to use i32.atomic.load when memory is shared"
    );
}
