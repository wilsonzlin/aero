use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{mask_bits, CpuMode, CpuState};
use aero_cpu_core::Exception;
use aero_perf::{PerfCounters, PerfWorker};
use aero_x86::{Mnemonic, Register};
use std::sync::Arc;

const CODE_ADDR: u64 = 0;

fn setup_bus() -> FlatTestBus {
    FlatTestBus::new(0x10_000)
}

fn is_string_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Movsb
            | Mnemonic::Movsw
            | Mnemonic::Movsd
            | Mnemonic::Movsq
            | Mnemonic::Stosb
            | Mnemonic::Stosw
            | Mnemonic::Stosd
            | Mnemonic::Stosq
            | Mnemonic::Lodsb
            | Mnemonic::Lodsw
            | Mnemonic::Lodsd
            | Mnemonic::Lodsq
            | Mnemonic::Cmpsb
            | Mnemonic::Cmpsw
            | Mnemonic::Cmpsd
            | Mnemonic::Cmpsq
            | Mnemonic::Scasb
            | Mnemonic::Scasw
            | Mnemonic::Scasd
            | Mnemonic::Scasq
    )
}

fn has_addr_size_override(bytes: &[u8], bitness: u32) -> bool {
    let mut i = 0usize;
    let mut seen = false;
    while i < bytes.len() {
        let b = bytes[i];
        let is_legacy_prefix = matches!(
            b,
            0xF0 | 0xF2 | 0xF3 // lock/rep
                | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 // segment overrides
                | 0x66 // operand-size override
                | 0x67 // address-size override
        );
        let is_rex = bitness == 64 && (0x40..=0x4F).contains(&b);
        if !(is_legacy_prefix || is_rex) {
            break;
        }
        if b == 0x67 {
            seen = true;
        }
        i += 1;
    }
    seen
}

fn effective_addr_size(bitness: u32, addr_size_override: bool) -> u32 {
    match bitness {
        16 => {
            if addr_size_override {
                32
            } else {
                16
            }
        }
        32 => {
            if addr_size_override {
                16
            } else {
                32
            }
        }
        64 => {
            if addr_size_override {
                32
            } else {
                64
            }
        }
        _ => 64,
    }
}

fn string_count_reg(addr_bits: u32) -> Register {
    match addr_bits {
        16 => Register::CX,
        32 => Register::ECX,
        _ => Register::RCX,
    }
}

fn exec_bytes_counted(
    state: &mut CpuState,
    bus: &mut FlatTestBus,
    bytes: &[u8],
    perf: &mut PerfWorker,
) -> Result<(), Exception> {
    bus.load(CODE_ADDR, bytes);
    state.segments.cs.base = 0;
    state.set_rip(CODE_ADDR);

    let decoded = aero_x86::decode(bytes, CODE_ADDR, state.bitness())
        .map_err(|_| Exception::InvalidOpcode)?;
    let instr = &decoded.instr;

    let is_rep = instr.has_rep_prefix() || instr.has_repne_prefix();
    let is_string = is_string_mnemonic(instr.mnemonic());

    let addr_bits = effective_addr_size(
        state.bitness(),
        has_addr_size_override(bytes, state.bitness()),
    );
    let count_reg = string_count_reg(addr_bits);
    let count_mask = mask_bits(addr_bits);

    let count_before = if is_rep && is_string {
        state.read_reg(count_reg) & count_mask
    } else {
        0
    };

    let res = run_batch(state, bus, 1);
    match res.exit {
        BatchExit::Completed => {}
        BatchExit::Exception(e) => return Err(e),
        BatchExit::CpuExit(e) => panic!("unexpected cpu exit: {e:?}"),
        other => panic!("unexpected tier0 exit: {other:?}"),
    }

    perf.retire_instructions(1);
    if is_rep && is_string {
        let count_after = state.read_reg(count_reg) & count_mask;
        let iterations = count_before.wrapping_sub(count_after) & count_mask;
        perf.add_rep_iterations(iterations);
    }

    Ok(())
}

#[test]
fn execute_bytes_counted_increments_instruction_counter() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::SI, 0x10);
    state.write_reg(Register::DI, 0x20);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0xAA).unwrap();

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    exec_bytes_counted(&mut state, &mut bus, &[0xA4], &mut perf).unwrap(); // MOVSB

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 1);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 0);
}

#[test]
fn rep_string_iterations_are_tracked_separately() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::SI, 0x10);
    state.write_reg(Register::DI, 0x20);
    state.write_reg(Register::CX, 3);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0x11).unwrap();
    bus.write_u8(0x1000 + 0x11, 0x22).unwrap();
    bus.write_u8(0x1000 + 0x12, 0x33).unwrap();

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    exec_bytes_counted(&mut state, &mut bus, &[0xF3, 0xA4], &mut perf).unwrap(); // REP MOVSB

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 1);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 3);
    assert_eq!(state.read_reg(Register::CX), 0);
}

#[test]
fn repe_cmpsb_reports_actual_iterations_executed() {
    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.write_reg(Register::ESI, 0x10);
    state.write_reg(Register::EDI, 0x20);
    state.write_reg(Register::ECX, 5);

    let mut bus = setup_bus();
    for i in 0..5u64 {
        bus.write_u8(0x1000 + 0x10 + i, if i == 3 { 0x99 } else { i as u8 })
            .unwrap();
        bus.write_u8(0x2000 + 0x20 + i, i as u8).unwrap();
    }

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    exec_bytes_counted(&mut state, &mut bus, &[0xF3, 0xA6], &mut perf).unwrap(); // REPE CMPSB

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 1);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 4);
    assert_eq!(state.read_reg(Register::ECX), 1);
}
