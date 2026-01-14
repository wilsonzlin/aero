#![cfg(all(debug_assertions, not(target_arch = "wasm32")))]

mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::interp::execute_block;
use aero_jit_x86::tier1::ir::{BinOp, GuestReg, IrBlock, IrBuilder, IrTerminator, ValueId};
use aero_jit_x86::tier1::{Tier1WasmCodegen, EXPORT_TIER1_BLOCK_FN};
use aero_jit_x86::wasm::{
    IMPORT_JIT_EXIT, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE, IMPORT_PAGE_FAULT, JIT_EXIT_SENTINEL_I64,
};
use aero_types::{Cond, Flag, FlagSet, Gpr, Width};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot, SimpleBus};
use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const CPU_PTR: i32 = 0x1_0000;
const JIT_CTX_PTR: i32 = CPU_PTR + abi::CPU_STATE_SIZE as i32;

// Page 0 is guest RAM. Keep it one page so we can cheaply compare the entire region and still
// catch stray/out-of-window writes due to codegen bugs.
const RAM_SIZE: usize = 0x1_0000;

// Keep random load/store addresses in a smaller window so we can safely generate in-bounds
// accesses without having to prove address arithmetic properties.
const RAM_WINDOW_BASE: u64 = 0x2000;
const RAM_WINDOW_LEN: u64 = 0x1000;

const MAX_IR_INSTS: usize = 50;
const ITERS: usize = 300;
const SEED: u64 = 0x6c0a_9f3a_77d5_1d2b;

fn validate_wasm(bytes: &[u8]) {
    let mut validator = wasmparser::Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn define_mem_helpers(store: &mut Store<()>, linker: &mut Linker<()>, memory: Memory) {
    fn read<const N: usize>(caller: &mut Caller<'_, ()>, memory: &Memory, addr: usize) -> u64 {
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
        caller: &mut Caller<'_, ()>,
        memory: &Memory,
        addr: usize,
        value: u64,
    ) {
        let mut buf = vec![0u8; N];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (value >> (i * 8)) as u8;
        }
        memory
            .write(caller, addr, &buf)
            .expect("memory write in bounds");
    }

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<1>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<2>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<4>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U64,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i64 {
                    read::<8>(&mut caller, &mem, addr as usize) as i64
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<1>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<2>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<4>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U64,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i64| {
                    write::<8>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();
}

fn instantiate(engine: &Engine, bytes: &[u8]) -> (Store<()>, Memory, TypedFunc<(i32, i32), i64>) {
    let module = Module::new(engine, bytes).unwrap();

    let mut store = Store::new(engine, ());
    let mut linker = Linker::new(engine);

    // Guest memory in page 0, CpuState at CPU_PTR in page 1, and room for the JIT context.
    let memory = Memory::new(&mut store, MemoryType::new(4, None)).unwrap();
    linker.define(IMPORT_MODULE, IMPORT_MEMORY, memory).unwrap();

    define_mem_helpers(&mut store, &mut linker, memory);

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 {
                    panic!("page_fault should not be called by tier1_random_ir_equiv tests");
                },
            ),
        )
        .unwrap();

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, ()>, _kind: i32, rip: i64| -> i64 { rip },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let block = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TIER1_BLOCK_FN)
        .unwrap();
    (store, memory, block)
}

fn run_wasm(
    engine: &Engine,
    ir: &IrBlock,
    cpu: &CpuState,
    bus: &SimpleBus,
) -> (i64, CpuSnapshot, Vec<u8>) {
    let wasm = Tier1WasmCodegen::new().compile_block(ir);
    validate_wasm(&wasm);

    let (mut store, memory, func) = instantiate(engine, &wasm);

    // Initialize guest memory.
    memory.write(&mut store, 0, bus.mem()).unwrap();

    // Initialize CpuState.
    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(cpu, &mut cpu_bytes);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();

    // Read back guest memory region (page 0).
    let mut out_mem = vec![0u8; bus.mem().len()];
    memory.read(&store, 0, &mut out_mem).unwrap();

    // Read back CpuState.
    let mut out_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut out_cpu_bytes)
        .unwrap();
    let out_cpu = CpuSnapshot::from_wasm_bytes(&out_cpu_bytes);

    (ret, out_cpu, out_mem)
}

fn run_interp(ir: &IrBlock, cpu: &CpuState, bus: &SimpleBus) -> CpuSnapshotAndMem {
    let mut interp_bus = bus.clone();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(cpu, &mut cpu_bytes);

    let _ = execute_block(ir, &mut cpu_bytes, &mut interp_bus);

    CpuSnapshotAndMem {
        cpu: CpuSnapshot::from_wasm_bytes(&cpu_bytes),
        mem: interp_bus.mem().to_vec(),
    }
}

struct CpuSnapshotAndMem {
    cpu: CpuSnapshot,
    mem: Vec<u8>,
}

struct GenCtx {
    b: IrBuilder,
    insts: usize,
    // Value ids, plus their declared type.
    values: Vec<(ValueId, Width)>,
    // W8 values that are known to be boolean-like (0 or 1). The Tier-1 IR interpreter treats
    // condition values as `v & 1 != 0`, while Tier-1 WASM codegen currently uses `v != 0`.
    // Translation only produces 0/1 conditions, so keep the random generator consistent with that
    // contract to avoid false positives.
    bool_values: Vec<ValueId>,
    // Known-safe guest addresses within the RAM window, always safe for 8-byte accesses.
    addr_values: Vec<ValueId>,
}

impl GenCtx {
    fn new(entry_rip: u64) -> Self {
        Self {
            b: IrBuilder::new(entry_rip),
            insts: 0,
            values: Vec::new(),
            bool_values: Vec::new(),
            addr_values: Vec::new(),
        }
    }

    fn can_add(&self, n: usize) -> bool {
        self.insts + n <= MAX_IR_INSTS
    }

    fn record(&mut self, id: ValueId, width: Width) {
        self.values.push((id, width));
    }

    fn record_bool(&mut self, id: ValueId) {
        self.bool_values.push(id);
    }

    fn const_int(&mut self, width: Width, value: u64) -> ValueId {
        self.insts += 1;
        let id = self.b.const_int(width, value);
        self.record(id, width);
        if width == Width::W8 && (value & 0xff) <= 1 {
            self.record_bool(id);
        }
        id
    }

    fn read_reg(&mut self, reg: GuestReg) -> ValueId {
        let width = match reg {
            GuestReg::Gpr { width, .. } => width,
            GuestReg::Flag(_) => Width::W8,
            GuestReg::Rip => Width::W64,
        };
        self.insts += 1;
        let id = self.b.read_reg(reg);
        self.record(id, width);
        if matches!(reg, GuestReg::Flag(_)) {
            self.record_bool(id);
        }
        id
    }

    fn write_reg(&mut self, reg: GuestReg, src: ValueId) {
        self.insts += 1;
        self.b.write_reg(reg, src);
    }

    fn trunc(&mut self, width: Width, src: ValueId) -> ValueId {
        self.insts += 1;
        let id = self.b.trunc(width, src);
        self.record(id, width);
        id
    }

    fn load(&mut self, width: Width, addr: ValueId) -> ValueId {
        self.insts += 1;
        let id = self.b.load(width, addr);
        self.record(id, width);
        id
    }

    fn store(&mut self, width: Width, addr: ValueId, src: ValueId) {
        self.insts += 1;
        self.b.store(width, addr, src);
    }

    fn binop(
        &mut self,
        op: BinOp,
        width: Width,
        lhs: ValueId,
        rhs: ValueId,
        flags: FlagSet,
    ) -> ValueId {
        self.insts += 1;
        let id = self.b.binop(op, width, lhs, rhs, flags);
        self.record(id, width);
        id
    }

    fn cmp_flags(&mut self, width: Width, lhs: ValueId, rhs: ValueId, flags: FlagSet) {
        self.insts += 1;
        self.b.cmp_flags(width, lhs, rhs, flags);
    }

    fn test_flags(&mut self, width: Width, lhs: ValueId, rhs: ValueId, flags: FlagSet) {
        self.insts += 1;
        self.b.test_flags(width, lhs, rhs, flags);
    }

    fn eval_cond(&mut self, cond: Cond) -> ValueId {
        self.insts += 1;
        let id = self.b.eval_cond(cond);
        self.record(id, Width::W8);
        self.record_bool(id);
        id
    }

    fn select(
        &mut self,
        width: Width,
        cond: ValueId,
        if_true: ValueId,
        if_false: ValueId,
    ) -> ValueId {
        self.insts += 1;
        let id = self.b.select(width, cond, if_true, if_false);
        self.record(id, width);
        id
    }

    fn finish(self, terminator: IrTerminator) -> IrBlock {
        self.b.finish(terminator)
    }

    fn pick_any_value(&self, rng: &mut impl Rng) -> (ValueId, Width) {
        let idx = rng.gen_range(0..self.values.len());
        self.values[idx]
    }

    fn pick_value_of_width(&self, rng: &mut impl Rng, width: Width) -> Option<ValueId> {
        // Reservoir sample without allocating.
        let mut chosen = None;
        let mut seen = 0usize;
        for (id, w) in &self.values {
            if *w != width {
                continue;
            }
            seen += 1;
            if rng.gen_range(0..seen) == 0 {
                chosen = Some(*id);
            }
        }
        chosen
    }

    fn pick_or_const(&mut self, rng: &mut impl Rng, width: Width) -> ValueId {
        if let Some(v) = self.pick_value_of_width(rng, width) {
            return v;
        }
        // Ensure progress even if the generator forgot to seed something.
        self.const_int(width, rng.gen())
    }

    fn pick_safe_addr(&self, rng: &mut impl Rng) -> ValueId {
        self.addr_values[rng.gen_range(0..self.addr_values.len())]
    }

    fn pick_bool(&mut self, rng: &mut impl Rng) -> ValueId {
        if !self.bool_values.is_empty() {
            return self.bool_values[rng.gen_range(0..self.bool_values.len())];
        }
        // Fall back to a new constant boolean if we somehow failed to seed boolean values.
        self.const_int(Width::W8, rng.gen_range(0..=1))
    }
}

fn random_width(rng: &mut impl Rng) -> Width {
    match rng.gen_range(0..4u8) {
        0 => Width::W8,
        1 => Width::W16,
        2 => Width::W32,
        3 => Width::W64,
        _ => unreachable!(),
    }
}

fn random_gpr(rng: &mut impl Rng) -> Gpr {
    Gpr::from_u4(rng.gen_range(0..16)).unwrap()
}

fn random_flag(rng: &mut impl Rng) -> Flag {
    match rng.gen_range(0..6u8) {
        0 => Flag::Cf,
        1 => Flag::Pf,
        2 => Flag::Af,
        3 => Flag::Zf,
        4 => Flag::Sf,
        5 => Flag::Of,
        _ => unreachable!(),
    }
}

fn random_cond(rng: &mut impl Rng) -> Cond {
    match rng.gen_range(0..16u8) {
        0 => Cond::O,
        1 => Cond::No,
        2 => Cond::B,
        3 => Cond::Ae,
        4 => Cond::E,
        5 => Cond::Ne,
        6 => Cond::Be,
        7 => Cond::A,
        8 => Cond::S,
        9 => Cond::Ns,
        10 => Cond::P,
        11 => Cond::Np,
        12 => Cond::L,
        13 => Cond::Ge,
        14 => Cond::Le,
        15 => Cond::G,
        _ => unreachable!(),
    }
}

fn random_binop(rng: &mut impl Rng) -> BinOp {
    match rng.gen_range(0..8u8) {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::And,
        3 => BinOp::Or,
        4 => BinOp::Xor,
        5 => BinOp::Shl,
        6 => BinOp::Shr,
        7 => BinOp::Sar,
        _ => unreachable!(),
    }
}

fn random_flagset(rng: &mut impl Rng) -> FlagSet {
    let mut set = FlagSet::EMPTY;
    let bits = [
        FlagSet::CF,
        FlagSet::PF,
        FlagSet::AF,
        FlagSet::ZF,
        FlagSet::SF,
        FlagSet::OF,
    ];
    for bit in bits {
        // Bias towards non-empty sets.
        if rng.gen_bool(0.4) {
            set = set.union(bit);
        }
    }
    set
}

fn random_guest_gpr_reg(rng: &mut impl Rng) -> GuestReg {
    let reg = random_gpr(rng);
    let width = random_width(rng);
    let high8 = width == Width::W8
        && matches!(reg, Gpr::Rax | Gpr::Rcx | Gpr::Rdx | Gpr::Rbx)
        && rng.gen_bool(0.25);
    GuestReg::Gpr { reg, width, high8 }
}

fn random_ir_block(rng: &mut impl Rng, entry_rip: u64) -> IrBlock {
    let mut ctx = GenCtx::new(entry_rip);

    // Seed one value of each width so later instructions can always find operands without having to
    // insert extra consts (keeping blocks bounded in size).
    for w in [Width::W8, Width::W16, Width::W32, Width::W64] {
        if !ctx.can_add(1) {
            break;
        }
        if w == Width::W8 {
            // Ensure we always have at least one boolean W8 value available for Select/CondJump.
            ctx.const_int(w, rng.gen_range(0..=1));
        } else {
            ctx.const_int(w, rng.gen());
        }
    }

    // Seed a few safe memory addresses (safe for 8-byte loads/stores).
    let addr_max = RAM_WINDOW_BASE + RAM_WINDOW_LEN - 8;
    for _ in 0..8 {
        if !ctx.can_add(1) {
            break;
        }
        let addr = rng.gen_range(RAM_WINDOW_BASE..=addr_max);
        let v = ctx.const_int(Width::W64, addr);
        ctx.addr_values.push(v);
    }

    // Ensure we always have at least one bool-like W8 value (condition/flag sources).
    if ctx.bool_values.is_empty() && ctx.can_add(1) {
        ctx.read_reg(GuestReg::Flag(random_flag(rng)));
    }

    while ctx.insts < MAX_IR_INSTS {
        // Stop early if we haven't seeded safe addresses for memory ops (shouldn't happen unless
        // MAX_IR_INSTS is very small).
        let have_addrs = !ctx.addr_values.is_empty();

        let choice: u8 = rng.gen_range(0..100);
        if choice < 8 {
            // Const
            if !ctx.can_add(1) {
                break;
            }
            let w = random_width(rng);
            ctx.const_int(w, rng.gen());
        } else if choice < 18 {
            // ReadReg (GPR/Flag)
            if !ctx.can_add(1) {
                break;
            }
            if rng.gen_bool(0.8) {
                let reg = random_guest_gpr_reg(rng);
                ctx.read_reg(reg);
            } else {
                ctx.read_reg(GuestReg::Flag(random_flag(rng)));
            }
        } else if choice < 28 {
            // WriteReg (GPR/Flag)
            if !ctx.can_add(1) {
                break;
            }
            if rng.gen_bool(0.8) {
                let reg = random_guest_gpr_reg(rng);
                let GuestReg::Gpr { width, .. } = reg else {
                    unreachable!()
                };
                let src = ctx.pick_or_const(rng, width);
                ctx.write_reg(reg, src);
            } else {
                let src = ctx.pick_bool(rng);
                ctx.write_reg(GuestReg::Flag(random_flag(rng)), src);
            }
        } else if choice < 36 {
            // Trunc
            if !ctx.can_add(1) {
                break;
            }
            let (src, src_w) = ctx.pick_any_value(rng);
            let dst_w = match src_w {
                Width::W8 => Width::W8,
                Width::W16 => {
                    if rng.gen_bool(0.5) {
                        Width::W8
                    } else {
                        Width::W16
                    }
                }
                Width::W32 => match rng.gen_range(0..3u8) {
                    0 => Width::W8,
                    1 => Width::W16,
                    2 => Width::W32,
                    _ => unreachable!(),
                },
                Width::W64 => match rng.gen_range(0..4u8) {
                    0 => Width::W8,
                    1 => Width::W16,
                    2 => Width::W32,
                    3 => Width::W64,
                    _ => unreachable!(),
                },
            };
            ctx.trunc(dst_w, src);
        } else if choice < 56 {
            // BinOp (all ops)
            if !ctx.can_add(1) {
                break;
            }
            let op = random_binop(rng);
            let w = random_width(rng);
            let lhs = ctx.pick_or_const(rng, w);
            let rhs = ctx.pick_or_const(rng, w);
            let flags = random_flagset(rng);
            ctx.binop(op, w, lhs, rhs, flags);
        } else if choice < 66 {
            // CmpFlags
            if !ctx.can_add(1) {
                break;
            }
            let w = random_width(rng);
            let lhs = ctx.pick_or_const(rng, w);
            let rhs = ctx.pick_or_const(rng, w);
            let flags = random_flagset(rng).union(FlagSet::ZF); // bias towards ZF usage
            ctx.cmp_flags(w, lhs, rhs, flags);
        } else if choice < 72 {
            // TestFlags
            if !ctx.can_add(1) {
                break;
            }
            let w = random_width(rng);
            let lhs = ctx.pick_or_const(rng, w);
            let rhs = ctx.pick_or_const(rng, w);
            let flags = random_flagset(rng).union(FlagSet::PF);
            ctx.test_flags(w, lhs, rhs, flags);
        } else if choice < 78 {
            // EvalCond
            if !ctx.can_add(1) {
                break;
            }
            ctx.eval_cond(random_cond(rng));
        } else if choice < 88 {
            // Select
            if !ctx.can_add(1) {
                break;
            }
            let w = random_width(rng);
            let cond = ctx.pick_bool(rng);
            let if_true = ctx.pick_or_const(rng, w);
            let if_false = ctx.pick_or_const(rng, w);
            ctx.select(w, cond, if_true, if_false);
        } else if choice < 94 {
            // Load
            if !have_addrs || !ctx.can_add(1) {
                continue;
            }
            let w = random_width(rng);
            let addr = ctx.pick_safe_addr(rng);
            ctx.load(w, addr);
        } else {
            // Store
            if !have_addrs || !ctx.can_add(1) {
                continue;
            }
            let w = random_width(rng);
            let addr = ctx.pick_safe_addr(rng);
            let src = ctx.pick_or_const(rng, w);
            ctx.store(w, addr, src);
        }
    }

    // Random-ish terminator without inserting new values.
    let term_choice: u8 = rng.gen_range(0..100);
    let term = if term_choice < 50 {
        IrTerminator::Jump {
            target: entry_rip.wrapping_add(rng.gen_range(0..0x1000u64)),
        }
    } else if term_choice < 80 {
        let cond = ctx.pick_bool(rng);
        IrTerminator::CondJump {
            cond,
            target: entry_rip.wrapping_add(rng.gen_range(0..0x1000u64)),
            fallthrough: entry_rip.wrapping_add(rng.gen_range(0..0x1000u64)),
        }
    } else if term_choice < 95 {
        let (v, _w) = ctx.pick_any_value(rng);
        IrTerminator::IndirectJump { target: v }
    } else {
        IrTerminator::ExitToInterpreter {
            next_rip: entry_rip.wrapping_add(rng.gen_range(0..0x1000u64)),
        }
    };

    let block = ctx.finish(term);
    block
        .validate()
        .unwrap_or_else(|e| panic!("generated invalid IR: {e}\n{}", block.to_text()));
    block
}

fn format_cpu_diff(expected: &CpuSnapshot, actual: &CpuSnapshot) -> String {
    let mut out = String::new();

    if expected.rip != actual.rip {
        out.push_str(&format!(
            "  rip: expected=0x{:016x} actual=0x{:016x}\n",
            expected.rip, actual.rip
        ));
    }
    if expected.rflags != actual.rflags {
        out.push_str(&format!(
            "  rflags: expected=0x{:016x} actual=0x{:016x} (xor=0x{:016x})\n",
            expected.rflags,
            actual.rflags,
            expected.rflags ^ actual.rflags
        ));
    }

    for i in 0..16 {
        if expected.gpr[i] == actual.gpr[i] {
            continue;
        }
        let reg = Gpr::from_u4(i as u8).unwrap();
        out.push_str(&format!(
            "  {reg}: expected=0x{:016x} actual=0x{:016x}\n",
            expected.gpr[i], actual.gpr[i]
        ));
    }

    out
}

fn format_mem_diff(expected: &[u8], actual: &[u8]) -> String {
    if expected.len() != actual.len() {
        return format!(
            "  len: expected={} actual={}\n",
            expected.len(),
            actual.len()
        );
    }

    if let Some((idx, (a, b))) = expected
        .iter()
        .copied()
        .zip(actual.iter().copied())
        .enumerate()
        .find(|(_, (a, b))| a != b)
    {
        let window_start = idx.saturating_sub(8);
        let window_end = (idx + 8).min(expected.len());
        let exp = &expected[window_start..window_end];
        let act = &actual[window_start..window_end];
        return format!(
            "  first_diff: off=0x{idx:04x} expected=0x{a:02x} actual=0x{b:02x}\n  window[0x{window_start:04x}..0x{window_end:04x}]:\n    expected: {exp:02x?}\n    actual:   {act:02x?}\n",
        );
    }

    String::new()
}

fn random_cpu_state(rng: &mut impl Rng, entry_rip: u64) -> CpuState {
    let mut cpu = CpuState {
        rip: entry_rip,
        ..Default::default()
    };

    for slot in &mut cpu.gpr {
        *slot = rng.gen();
    }

    cpu.set_rflags(rng.gen::<u64>() | abi::RFLAGS_RESERVED1);

    cpu
}

fn random_bus(rng: &mut impl Rng) -> SimpleBus {
    let mut bus = SimpleBus::new(RAM_SIZE);
    let mut bytes = vec![0u8; RAM_SIZE];
    rng.fill(&mut bytes[..]);
    bus.load(0, &bytes);
    bus
}

#[test]
fn tier1_random_ir_matches_wasm_codegen() {
    // Keep the wasmi engine shared across iterations for performance.
    let engine = Engine::default();

    for iter in 0..ITERS {
        // Per-iteration RNG so failures can be reproduced by iteration number without depending on
        // prior test cases.
        let mut rng =
            ChaCha8Rng::seed_from_u64(SEED ^ (iter as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));

        let entry_rip = 0x1000u64;
        let cpu = random_cpu_state(&mut rng, entry_rip);
        let bus = random_bus(&mut rng);

        let ir = random_ir_block(&mut rng, entry_rip);

        let interp = run_interp(&ir, &cpu, &bus);
        let (ret, wasm_cpu, wasm_mem) = run_wasm(&engine, &ir, &cpu, &bus);

        let wasm_next_rip = if ret == JIT_EXIT_SENTINEL_I64 {
            wasm_cpu.rip as i64
        } else {
            ret
        };
        if wasm_next_rip as u64 != wasm_cpu.rip {
            panic!(
                "tier1_random_ir_equiv internal error: ret/CPU.RIP mismatch at iter={iter}\nret={ret} cpu.rip=0x{:x}\n{}",
                wasm_cpu.rip,
                ir.to_text()
            );
        }

        if interp.cpu != wasm_cpu || interp.mem != wasm_mem {
            let cpu_diff = format_cpu_diff(&interp.cpu, &wasm_cpu);
            let mem_diff = format_mem_diff(&interp.mem, &wasm_mem);
            panic!(
                "tier1 random IR equivalence failed\n  iter={iter}\n  seed=0x{SEED:016x}\n\nIR:\n{}\nCPU diff:\n{}\nMemory diff:\n{}",
                ir.to_text(),
                cpu_diff,
                mem_diff
            );
        }
    }
}
