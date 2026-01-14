use std::collections::{BTreeSet, HashSet};

use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::{BinOp, GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::tier1::Tier1WasmOptions;
use aero_types::{FlagSet, Gpr, Width};

fn collect_gpr_load_store_offsets(wasm: &[u8]) -> (BTreeSet<u64>, BTreeSet<u64>) {
    let gpr_offsets: HashSet<u64> = abi::CPU_GPR_OFF.iter().copied().map(u64::from).collect();
    let mut loads: BTreeSet<u64> = BTreeSet::new();
    let mut stores: BTreeSet<u64> = BTreeSet::new();

    #[derive(Clone, Copy)]
    enum PrevOp {
        LocalGet(u32),
        Other,
    }

    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.unwrap();
        if let wasmparser::Payload::CodeSectionEntry(body) = payload {
            let mut ops = body.get_operators_reader().unwrap();
            let mut prev0 = PrevOp::Other;
            let mut prev1 = PrevOp::Other;
            while !ops.eof() {
                let op = ops.read().unwrap();
                match op {
                    wasmparser::Operator::I64Load { memarg } => {
                        // Only count CpuState GPR loads (base pointer is cpu_ptr local 0). This
                        // avoids false positives from inline-TLB structures that also use offsets
                        // 0/8/etc.
                        if gpr_offsets.contains(&memarg.offset)
                            && matches!(prev0, PrevOp::LocalGet(0))
                        {
                            loads.insert(memarg.offset);
                        }
                    }
                    wasmparser::Operator::I64Store { memarg } => {
                        if gpr_offsets.contains(&memarg.offset)
                            && matches!(prev1, PrevOp::LocalGet(0))
                        {
                            stores.insert(memarg.offset);
                        }
                    }
                    _ => {}
                }

                let cur = match op {
                    wasmparser::Operator::LocalGet { local_index } => PrevOp::LocalGet(local_index),
                    _ => PrevOp::Other,
                };
                prev1 = prev0;
                prev0 = cur;
            }
        }
    }

    (loads, stores)
}

fn collect_cpu_ptr_i64_load_store_offsets(wasm: &[u8]) -> (BTreeSet<u64>, BTreeSet<u64>) {
    let mut loads: BTreeSet<u64> = BTreeSet::new();
    let mut stores: BTreeSet<u64> = BTreeSet::new();

    #[derive(Clone, Copy)]
    enum PrevOp {
        LocalGet(u32),
        Other,
    }

    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.unwrap();
        if let wasmparser::Payload::CodeSectionEntry(body) = payload {
            let mut ops = body.get_operators_reader().unwrap();
            let mut prev0 = PrevOp::Other;
            let mut prev1 = PrevOp::Other;
            while !ops.eof() {
                let op = ops.read().unwrap();
                match op {
                    wasmparser::Operator::I64Load { memarg } => {
                        if matches!(prev0, PrevOp::LocalGet(0)) {
                            loads.insert(memarg.offset);
                        }
                    }
                    wasmparser::Operator::I64Store { memarg } => {
                        if matches!(prev1, PrevOp::LocalGet(0)) {
                            stores.insert(memarg.offset);
                        }
                    }
                    _ => {}
                };

                let cur = match op {
                    wasmparser::Operator::LocalGet { local_index } => PrevOp::LocalGet(local_index),
                    _ => PrevOp::Other,
                };
                prev1 = prev0;
                prev0 = cur;
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

#[cfg(feature = "tier1-inline-tlb")]
#[test]
fn tier1_codegen_early_exit_loads_regs_spilled_after_inline_tlb_exit_point() {
    let entry = 0x1000u64;

    // Blocks using the inline-TLB fast path can exit early on MMIO. If a GPR is spilled but its
    // first write is after an instruction that may exit, Tier-1 must still load the initial value
    // so the epilogue doesn't clobber the architectural register with the default zero local.
    let mut b = IrBuilder::new(entry);
    let addr = b.const_int(Width::W64, 0xF000);
    let _ = b.load(Width::W32, addr);

    let v = b.const_int(Width::W64, 0x1234);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v,
    );

    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let rbx_off = abi::CPU_GPR_OFF[Gpr::Rbx.as_u8() as usize] as u64;
    let (loads, stores) = collect_gpr_load_store_offsets(&wasm);
    assert_eq!(loads, BTreeSet::from([rbx_off]));
    assert_eq!(stores, BTreeSet::from([rbx_off]));
}

#[cfg(feature = "tier1-inline-tlb")]
#[test]
fn tier1_codegen_inline_tlb_mmio_fallback_does_not_force_gpr_or_rip_loads() {
    let entry = 0x1000u64;

    // When inline-TLB is enabled but `inline_tlb_mmio_exit` is disabled, memory ops should fall
    // back to the slow helpers for non-RAM translations but continue executing the block (no
    // early-exit `br`). In this mode, later full-width writes should not force prologue loads of
    // those GPRs.
    let mut b = IrBuilder::new(entry);
    let addr = b.const_int(Width::W64, 0xF000);
    let _ = b.load(Width::W32, addr);

    let v = b.const_int(Width::W64, 0x1234);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v,
    );

    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    let rbx_off = abi::CPU_GPR_OFF[Gpr::Rbx.as_u8() as usize] as u64;
    let (gpr_loads, gpr_stores) = collect_gpr_load_store_offsets(&wasm);
    assert!(
        !gpr_loads.contains(&rbx_off),
        "RBX should not be loaded when inline_tlb_mmio_exit=false"
    );
    assert_eq!(gpr_stores, BTreeSet::from([rbx_off]));

    // In the MMIO-fallback configuration, Tier-1 shouldn't need to load RIP at block entry either
    // (it is only needed to report MMIO exits / helper-call bailouts).
    let (cpu_ptr_i64_loads, _cpu_ptr_i64_stores) = collect_cpu_ptr_i64_load_store_offsets(&wasm);
    assert!(
        !cpu_ptr_i64_loads.contains(&(abi::CPU_RIP_OFF as u64)),
        "RIP should not be loaded when inline_tlb_mmio_exit=false"
    );
}

#[test]
fn tier1_codegen_read_only_flags_loads_without_spill() {
    use aero_types::Cond;

    let entry = 0x1000u64;
    let mut b = IrBuilder::new(entry);
    let _ = b.eval_cond(Cond::E);
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let (loads, stores) = collect_cpu_ptr_i64_load_store_offsets(&wasm);
    assert!(
        loads.contains(&(abi::CPU_RFLAGS_OFF as u64)),
        "expected CpuState.rflags to be loaded when flags are read"
    );
    assert!(
        !stores.contains(&(abi::CPU_RFLAGS_OFF as u64)),
        "expected CpuState.rflags not to be spilled when flags are only read"
    );
}

#[test]
fn tier1_codegen_flag_writes_spill_rflags() {
    let entry = 0x1000u64;
    let mut b = IrBuilder::new(entry);
    let lhs = b.const_int(Width::W64, 1);
    let rhs = b.const_int(Width::W64, 2);
    let _ = b.binop(BinOp::Add, Width::W64, lhs, rhs, FlagSet::ALU);
    let block = b.finish(IrTerminator::Jump { target: entry + 1 });
    block.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&block);
    let (loads, stores) = collect_cpu_ptr_i64_load_store_offsets(&wasm);
    assert!(
        loads.contains(&(abi::CPU_RFLAGS_OFF as u64)),
        "expected CpuState.rflags to be loaded when flags are written"
    );
    assert!(
        stores.contains(&(abi::CPU_RFLAGS_OFF as u64)),
        "expected CpuState.rflags to be spilled when flags are written"
    );
}
