use std::collections::{BTreeSet, HashSet};

use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::{BinOp, GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
use aero_types::{FlagSet, Gpr, Width};

fn collect_gpr_load_store_offsets(wasm: &[u8]) -> (BTreeSet<u64>, BTreeSet<u64>) {
    let gpr_offsets: HashSet<u64> = abi::CPU_GPR_OFF.iter().copied().map(u64::from).collect();
    let mut loads: BTreeSet<u64> = BTreeSet::new();
    let mut stores: BTreeSet<u64> = BTreeSet::new();

    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.unwrap();
        if let wasmparser::Payload::CodeSectionEntry(body) = payload {
            let mut ops = body.get_operators_reader().unwrap();
            while !ops.eof() {
                match ops.read().unwrap() {
                    wasmparser::Operator::I64Load { memarg } => {
                        if gpr_offsets.contains(&memarg.offset) {
                            loads.insert(memarg.offset);
                        }
                    }
                    wasmparser::Operator::I64Store { memarg } => {
                        if gpr_offsets.contains(&memarg.offset) {
                            stores.insert(memarg.offset);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    (loads, stores)
}

#[test]
fn tier1_codegen_spills_only_written_gprs() {
    let entry = 0x1000u64;

    // Minimal block that only touches RAX:
    //   rax = rax + 1
    let mut b = IrBuilder::new(entry);
    let rax_in = b.read_reg(GuestReg::Gpr {
        reg: Gpr::Rax,
        width: Width::W64,
        high8: false,
    });
    let one = b.const_int(Width::W64, 1);
    let rax_out = b.binop(BinOp::Add, Width::W64, rax_in, one, FlagSet::EMPTY);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W64,
            high8: false,
        },
        rax_out,
    );
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);

    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert_eq!(loads, BTreeSet::from([rax_off]));
    assert_eq!(stores, BTreeSet::from([rax_off]));
}

#[test]
fn tier1_codegen_full_width_write_does_not_force_load() {
    let entry = 0x1000u64;

    let mut b = IrBuilder::new(entry);
    let v = b.const_int(Width::W64, 0x1234);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W64,
            high8: false,
        },
        v,
    );
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert!(loads.is_empty());
    assert_eq!(stores, BTreeSet::from([rax_off]));
}

#[test]
fn tier1_codegen_32bit_write_does_not_force_load() {
    let entry = 0x1000u64;

    let mut b = IrBuilder::new(entry);
    let v = b.const_int(Width::W32, 0x1234_5678);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
            high8: false,
        },
        v,
    );
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert!(loads.is_empty());
    assert_eq!(stores, BTreeSet::from([rax_off]));
}

#[test]
fn tier1_codegen_partial_write_forces_load() {
    let entry = 0x1000u64;

    let mut b = IrBuilder::new(entry);
    let v = b.const_int(Width::W8, 0xab);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W8,
            high8: false,
        },
        v,
    );
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert_eq!(loads, BTreeSet::from([rax_off]));
    assert_eq!(stores, BTreeSet::from([rax_off]));
}

#[test]
fn tier1_codegen_read_only_loads_without_spill() {
    let entry = 0x1000u64;

    let mut b = IrBuilder::new(entry);
    let _ = b.read_reg(GuestReg::Gpr {
        reg: Gpr::Rax,
        width: Width::W64,
        high8: false,
    });
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert_eq!(loads, BTreeSet::from([rax_off]));
    assert!(stores.is_empty());
}

#[test]
fn tier1_codegen_high8_write_forces_load() {
    let entry = 0x1000u64;

    let mut b = IrBuilder::new(entry);
    let v = b.const_int(Width::W8, 0x12);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W8,
            high8: true,
        },
        v,
    );
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert_eq!(loads, BTreeSet::from([rax_off]));
    assert_eq!(stores, BTreeSet::from([rax_off]));
}

#[test]
fn tier1_codegen_16bit_write_forces_load() {
    let entry = 0x1000u64;

    let mut b = IrBuilder::new(entry);
    let v = b.const_int(Width::W16, 0x1234);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W16,
            high8: false,
        },
        v,
    );
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert_eq!(loads, BTreeSet::from([rax_off]));
    assert_eq!(stores, BTreeSet::from([rax_off]));
}
