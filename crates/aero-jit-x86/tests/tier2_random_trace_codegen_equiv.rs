#![cfg(not(target_arch = "wasm32"))]

use rand::{seq::SliceRandom, Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};
use wasmparser::Validator;

mod tier1_common;

use aero_jit_x86::abi;
use aero_jit_x86::tier2::interp::{run_trace_with_cached_regs, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::{BinOp, FlagValues, Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};
use aero_jit_x86::tier2::wasm_codegen::{Tier2WasmCodegen, EXPORT_TRACE_FN};
use aero_jit_x86::wasm::{
    IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE,
};

use aero_types::{Flag, FlagSet, Gpr, Width};
use tier1_common::SimpleBus;

const CPU_PTR: i32 = 0x1_0000;
const JIT_CTX_PTR: i32 = CPU_PTR + (abi::CPU_STATE_SIZE as i32);
const GUEST_MEM_SIZE: usize = 0x1_0000; // 64KiB, page 0
const INIT_RIP: u64 = 0x1234_5678;

fn validate_wasm(bytes: &[u8]) {
    let mut validator = Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn v(idx: u32) -> ValueId {
    ValueId(idx)
}

fn make_random_state(rng: &mut ChaCha8Rng) -> T2State {
    let mut state = T2State::default();
    for reg in aero_jit_x86::tier2::ir::ALL_GPRS {
        state.cpu.gpr[reg.as_u8() as usize] = rng.gen();
    }
    state.cpu.rip = INIT_RIP;
    state.cpu.rflags = abi::RFLAGS_RESERVED1;
    // Randomize the Tier-2-observable flag subset.
    for flag in [Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of] {
        if rng.gen() {
            state.cpu.rflags |= 1u64 << flag.rflags_bit();
        }
    }
    state
}

fn gen_operand(rng: &mut ChaCha8Rng, values: &[ValueId]) -> Operand {
    if !values.is_empty() && rng.gen_bool(0.7) {
        Operand::Value(values[rng.gen_range(0..values.len())])
    } else {
        Operand::Const(rng.gen())
    }
}

fn safe_mem_addr(rng: &mut ChaCha8Rng, width: Width) -> u64 {
    let bytes = match width {
        Width::W8 => 1usize,
        Width::W16 => 2usize,
        Width::W32 => 4usize,
        Width::W64 => 8usize,
    };
    let max = GUEST_MEM_SIZE
        .checked_sub(bytes)
        .expect("guest memory too small");
    rng.gen_range(0..=max as u64)
}

fn gen_width(rng: &mut ChaCha8Rng) -> Width {
    *[Width::W8, Width::W16, Width::W32, Width::W64]
        .choose(rng)
        .unwrap()
}

fn gen_flag_values(rng: &mut ChaCha8Rng) -> FlagValues {
    FlagValues {
        cf: rng.gen(),
        pf: rng.gen(),
        af: rng.gen(),
        zf: rng.gen(),
        sf: rng.gen(),
        of: rng.gen(),
    }
}

fn gen_random_trace(rng: &mut ChaCha8Rng, max_instrs: usize) -> TraceIr {
    let mut next_value: u32 = 0;
    let mut values: Vec<ValueId> = Vec::new();
    let mut addr_values: Vec<ValueId> = Vec::new();
    let mut body: Vec<Instr> = Vec::new();

    for _ in 0..max_instrs {
        match rng.gen_range(0..100u32) {
            // Constants.
            0..=12 => {
                let dst = v(next_value);
                next_value += 1;
                let value = rng.gen();
                body.push(Instr::Const { dst, value });
                values.push(dst);
            }
            // Safe address constant (kept in a separate pool so we can use Value operands for
            // memory operations without risking OOB).
            13..=18 => {
                let dst = v(next_value);
                next_value += 1;
                let value = safe_mem_addr(rng, Width::W64);
                body.push(Instr::Const { dst, value });
                values.push(dst);
                addr_values.push(dst);
            }
            // Register load.
            19..=32 => {
                let dst = v(next_value);
                next_value += 1;
                let reg = *aero_jit_x86::tier2::ir::ALL_GPRS.choose(rng).unwrap();
                body.push(Instr::LoadReg { dst, reg });
                values.push(dst);
            }
            // Flag load.
            33..=38 => {
                let dst = v(next_value);
                next_value += 1;
                let flag = *[Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of]
                    .choose(rng)
                    .unwrap();
                body.push(Instr::LoadFlag { dst, flag });
                values.push(dst);
            }
            // ALU.
            39..=66 => {
                if values.is_empty() {
                    continue;
                }
                let dst = v(next_value);
                next_value += 1;
                let op = match rng.gen_range(0..11u32) {
                    0 => BinOp::Add,
                    1 => BinOp::Sub,
                    2 => BinOp::Mul,
                    3 => BinOp::And,
                    4 => BinOp::Or,
                    5 => BinOp::Xor,
                    6 => BinOp::Shl,
                    7 => BinOp::Shr,
                    8 => BinOp::Sar,
                    9 => BinOp::Eq,
                    _ => BinOp::LtU,
                };
                let lhs = gen_operand(rng, &values);
                let rhs = gen_operand(rng, &values);
                let flags = if rng.gen_bool(0.35) {
                    FlagSet::ALU
                } else {
                    FlagSet::EMPTY
                };
                body.push(Instr::BinOp {
                    dst,
                    op,
                    lhs,
                    rhs,
                    flags,
                });
                values.push(dst);
            }
            // Address computation (not necessarily used for memory ops).
            67..=74 => {
                let dst = v(next_value);
                next_value += 1;
                let base = gen_operand(rng, &values);
                let index = gen_operand(rng, &values);
                let scale = *[1u8, 2, 4, 8].choose(rng).unwrap();
                let disp = rng.gen::<i32>() as i64;
                body.push(Instr::Addr {
                    dst,
                    base,
                    index,
                    scale,
                    disp,
                });
                values.push(dst);
            }
            // Memory store.
            75..=82 => {
                let width = gen_width(rng);
                let addr = if !addr_values.is_empty() && rng.gen_bool(0.7) {
                    Operand::Value(addr_values[rng.gen_range(0..addr_values.len())])
                } else {
                    Operand::Const(safe_mem_addr(rng, width))
                };
                let src = gen_operand(rng, &values);
                body.push(Instr::StoreMem { addr, src, width });
            }
            // Memory load.
            83..=90 => {
                let dst = v(next_value);
                next_value += 1;
                let width = gen_width(rng);
                let addr = if !addr_values.is_empty() && rng.gen_bool(0.7) {
                    Operand::Value(addr_values[rng.gen_range(0..addr_values.len())])
                } else {
                    Operand::Const(safe_mem_addr(rng, width))
                };
                body.push(Instr::LoadMem { dst, addr, width });
                values.push(dst);
            }
            // SetFlags.
            91..=94 => {
                let mut mask = FlagSet::EMPTY;
                for (bit, set) in [
                    (FlagSet::CF, rng.gen_bool(0.5)),
                    (FlagSet::PF, rng.gen_bool(0.5)),
                    (FlagSet::AF, rng.gen_bool(0.5)),
                    (FlagSet::ZF, rng.gen_bool(0.5)),
                    (FlagSet::SF, rng.gen_bool(0.5)),
                    (FlagSet::OF, rng.gen_bool(0.5)),
                ] {
                    if set {
                        mask = mask.union(bit);
                    }
                }
                if mask.is_empty() {
                    // Always set at least one bit so we meaningfully exercise SetFlags.
                    mask = FlagSet::CF;
                }
                body.push(Instr::SetFlags {
                    mask,
                    values: gen_flag_values(rng),
                });
            }
            // Guard (potential side exit).
            95..=98 => {
                if values.is_empty() {
                    continue;
                }
                let cond = gen_operand(rng, &values);
                let expected = rng.gen();
                let exit_rip = 0xDEAD_0000_0000_0000u64 | (rng.gen::<u16>() as u64);
                body.push(Instr::Guard {
                    cond,
                    expected,
                    exit_rip,
                });
            }
            // Unconditional side exit (terminator). Ensure it's last.
            _ => {
                let exit_rip = 0xBEEF_0000_0000_0000u64 | (rng.gen::<u16>() as u64);
                body.push(Instr::SideExit { exit_rip });
                break;
            }
        }
    }

    // Post-processing may append extra instructions to improve test coverage. If we ended with an
    // unconditional terminator, temporarily pop it so we can re-append it at the end (Tier-2 IR
    // requires `Instr::SideExit` to be the final instruction).
    let tail_side_exit = match body.last().copied() {
        Some(Instr::SideExit { exit_rip }) => {
            body.pop();
            Some(exit_rip)
        }
        _ => None,
    };

    // Ensure traces exercise regalloc caching reasonably often by forcing at least one reg touch.
    if !body
        .iter()
        .any(|i| matches!(i, Instr::LoadReg { .. } | Instr::StoreReg { .. }))
    {
        let dst = v(next_value);
        next_value += 1;
        body.push(Instr::LoadReg { dst, reg: Gpr::Rax });
        values.push(dst);
        body.push(Instr::StoreReg {
            reg: Gpr::Rax,
            src: Operand::Value(dst),
        });
    }

    // Occasionally force at least one memory op.
    if !body
        .iter()
        .any(|i| matches!(i, Instr::LoadMem { .. } | Instr::StoreMem { .. }))
        && rng.gen_bool(0.3)
    {
        let addr_val = v(next_value);
        next_value += 1;
        body.push(Instr::Const {
            dst: addr_val,
            value: safe_mem_addr(rng, Width::W64),
        });
        values.push(addr_val);
        addr_values.push(addr_val);

        let val = v(next_value);
        body.push(Instr::LoadReg {
            dst: val,
            reg: Gpr::Rbx,
        });
        values.push(val);

        body.push(Instr::StoreMem {
            addr: Operand::Value(addr_val),
            src: Operand::Value(val),
            width: Width::W64,
        });
    }

    if let Some(exit_rip) = tail_side_exit {
        body.push(Instr::SideExit { exit_rip });
    }

    TraceIr {
        prologue: Vec::new(),
        body,
        kind: TraceKind::Linear,
    }
}

#[derive(Clone, Debug, Default)]
struct HostEnv;

fn define_mem_helpers(store: &mut Store<HostEnv>, linker: &mut Linker<HostEnv>, memory: Memory) {
    fn read<const N: usize>(caller: &mut Caller<'_, HostEnv>, memory: &Memory, addr: usize) -> u64 {
        let mut buf = [0u8; N];
        memory
            .read(caller, addr, &mut buf)
            .expect("memory read in bounds");
        let mut v = 0u64;
        for (i, b) in buf.iter().enumerate() {
            v |= (*b as u64) << (i * 8);
        }
        v
    }

    fn write<const N: usize>(
        caller: &mut Caller<'_, HostEnv>,
        memory: &Memory,
        addr: usize,
        value: u64,
    ) {
        let mut buf = [0u8; N];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (value >> (i * 8)) as u8;
        }
        memory
            .write(caller, addr, &buf)
            .expect("memory write in bounds");
    }

    // Reads.
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U8,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i32 {
                        read::<1>(&mut caller, &mem, addr as usize) as i32
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U16,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i32 {
                        read::<2>(&mut caller, &mem, addr as usize) as i32
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U32,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i32 {
                        read::<4>(&mut caller, &mem, addr as usize) as i32
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U64,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i64 {
                        read::<8>(&mut caller, &mem, addr as usize) as i64
                    },
                ),
            )
            .unwrap();
    }

    // Writes.
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U8,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i32| {
                        write::<1>(&mut caller, &mem, addr as usize, value as u64);
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U16,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i32| {
                        write::<2>(&mut caller, &mem, addr as usize, value as u64);
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U32,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i32| {
                        write::<4>(&mut caller, &mem, addr as usize, value as u64);
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U64,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i64| {
                        write::<8>(&mut caller, &mem, addr as usize, value as u64);
                    },
                ),
            )
            .unwrap();
    }
}

fn instantiate_trace(
    engine: &Engine,
    wasm: &[u8],
) -> (Store<HostEnv>, Memory, TypedFunc<(i32, i32), i64>) {
    let module = Module::new(engine, wasm).unwrap();
    let mut store = Store::new(engine, HostEnv);
    let mut linker = Linker::new(engine);

    // Two pages: guest memory in page 0, CpuState + JIT context at CPU_PTR in page 1.
    let memory = Memory::new(&mut store, MemoryType::new(2, None)).unwrap();
    linker.define(IMPORT_MODULE, IMPORT_MEMORY, memory).unwrap();

    define_mem_helpers(&mut store, &mut linker, memory);

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let trace = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TRACE_FN)
        .unwrap();
    (store, memory, trace)
}

fn write_cpu_state(bytes: &mut [u8], cpu: &aero_cpu_core::state::CpuState) {
    assert!(
        bytes.len() >= abi::CPU_STATE_SIZE as usize,
        "cpu state buffer too small"
    );
    for (&off, reg) in abi::CPU_GPR_OFF.iter().zip(cpu.gpr.iter()) {
        let off = off as usize;
        bytes[off..off + 8].copy_from_slice(&reg.to_le_bytes());
    }
    bytes[abi::CPU_RIP_OFF as usize..abi::CPU_RIP_OFF as usize + 8]
        .copy_from_slice(&cpu.rip.to_le_bytes());
    bytes[abi::CPU_RFLAGS_OFF as usize..abi::CPU_RFLAGS_OFF as usize + 8]
        .copy_from_slice(&cpu.rflags.to_le_bytes());
}

fn read_u64_le(bytes: &[u8], off: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[off..off + 8]);
    u64::from_le_bytes(buf)
}

fn read_cpu_state(bytes: &[u8]) -> ([u64; 16], u64, u64) {
    let mut gpr = [0u64; 16];
    for (dst, off) in gpr.iter_mut().zip(abi::CPU_GPR_OFF.iter()) {
        *dst = read_u64_le(bytes, *off as usize);
    }
    let rip = read_u64_le(bytes, abi::CPU_RIP_OFF as usize);
    let rflags = read_u64_le(bytes, abi::CPU_RFLAGS_OFF as usize);
    (gpr, rip, rflags)
}

fn assert_mem_eq(expected: &[u8], got: &[u8], trace_idx: usize, trace: &TraceIr) {
    if expected == got {
        return;
    }
    let min = expected.len().min(got.len());
    let mut first = None;
    for i in 0..min {
        if expected[i] != got[i] {
            first = Some(i);
            break;
        }
    }
    panic!("guest memory mismatch at idx={trace_idx} first_diff={first:?}\ntrace={trace:#?}");
}

#[test]
fn tier2_wasm_codegen_matches_interpreter_on_random_traces() {
    let env = RuntimeEnv::default();
    let engine = Engine::default();
    let mut rng = ChaCha8Rng::seed_from_u64(0x5EED);

    for trace_idx in 0..150 {
        let trace = gen_random_trace(&mut rng, 60);

        let init_state = make_random_state(&mut rng);
        let mut init_mem = vec![0u8; GUEST_MEM_SIZE];
        rng.fill(init_mem.as_mut_slice());

        let mut opt_trace = trace.clone();
        let opt = optimize_trace(&mut opt_trace, &OptConfig::default());

        // ---- Interpreter (expected) ------------------------------------------------------------
        let mut interp_state = init_state.clone();
        let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
        bus.load(0, &init_mem);

        let expected = run_trace_with_cached_regs(
            &opt_trace,
            &env,
            &mut bus,
            &mut interp_state,
            1,
            &opt.regalloc.cached,
        );

        let expected_next_rip = match expected.exit {
            RunExit::Returned => interp_state.cpu.rip,
            RunExit::SideExit { next_rip } => next_rip,
            RunExit::Invalidate { next_rip } => next_rip,
            RunExit::StepLimit => panic!("linear traces should not hit step limit"),
        };

        // ---- WASM codegen + wasmi exec --------------------------------------------------------
        let wasm = Tier2WasmCodegen::new().compile_trace(&opt_trace, &opt.regalloc);
        validate_wasm(&wasm);

        let (mut store, memory, func) = instantiate_trace(&engine, &wasm);
        memory.write(&mut store, 0, &init_mem).unwrap();

        let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
        write_cpu_state(&mut cpu_bytes, &init_state.cpu);
        memory
            .write(&mut store, CPU_PTR as usize, &cpu_bytes)
            .unwrap();

        let got_next_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
        assert_eq!(
            got_next_rip, expected_next_rip,
            "next_rip mismatch at idx={trace_idx}\ntrace={trace:#?}\nopt_trace={opt_trace:#?}\nexpected_exit={:?}",
            expected.exit
        );

        let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
        memory
            .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
            .unwrap();
        let (got_gpr, got_rip, got_rflags) = read_cpu_state(&got_cpu_bytes);

        assert_eq!(
            got_gpr, interp_state.cpu.gpr,
            "gpr mismatch at idx={trace_idx}\ntrace={trace:#?}\nopt_trace={opt_trace:#?}\nexpected_exit={:?}",
            expected.exit
        );
        assert_eq!(
            got_rip, interp_state.cpu.rip,
            "rip mismatch at idx={trace_idx}\ntrace={trace:#?}\nopt_trace={opt_trace:#?}\nexpected_exit={:?}",
            expected.exit
        );
        assert_eq!(
            got_rflags, interp_state.cpu.rflags,
            "rflags mismatch at idx={trace_idx}\ntrace={trace:#?}\nopt_trace={opt_trace:#?}\nexpected_exit={:?}",
            expected.exit
        );

        let has_mem_ops = opt_trace
            .iter_instrs()
            .any(|i| matches!(i, Instr::LoadMem { .. } | Instr::StoreMem { .. }));
        if has_mem_ops {
            let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
            memory.read(&store, 0, &mut got_guest_mem).unwrap();
            assert_mem_eq(bus.mem(), &got_guest_mem, trace_idx, &opt_trace);
        }
    }
}
