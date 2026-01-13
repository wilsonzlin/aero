use std::collections::{BTreeSet, HashSet};

use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::{BinOp, GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
use aero_types::{FlagSet, Gpr, Width};

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

    let gpr_offsets: HashSet<u64> = abi::CPU_GPR_OFF.iter().copied().map(u64::from).collect();
    let mut seen: BTreeSet<u64> = BTreeSet::new();

    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        let payload = payload.unwrap();
        if let wasmparser::Payload::CodeSectionEntry(body) = payload {
            let mut ops = body.get_operators_reader().unwrap();
            while !ops.eof() {
                match ops.read().unwrap() {
                    wasmparser::Operator::I64Load { memarg }
                    | wasmparser::Operator::I64Store { memarg } => {
                        if gpr_offsets.contains(&memarg.offset) {
                            seen.insert(memarg.offset);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let rax_off = abi::CPU_GPR_OFF[Gpr::Rax.as_u8() as usize] as u64;
    assert_eq!(seen, BTreeSet::from([rax_off]));
}

