use aero_cpu_core::interp::tier0::exec::{step, StepExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_EM, CR0_MP, CR0_NE, CR0_TS, CR4_OSFXSR};
use aero_cpu_core::Exception;

fn init(code: &[u8]) -> (CpuState, FlatTestBus) {
    let mut bus = FlatTestBus::new(0x1000);
    bus.load(0, code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    (state, bus)
}

#[test]
fn sse_ts_raises_nm_and_clearing_ts_allows_execution() {
    // xorps xmm0, xmm1
    let code = [0x0F, 0x57, 0xC1];
    let (mut state, mut bus) = init(&code);
    state.control.cr4 |= CR4_OSFXSR;
    state.sse.xmm[0] = 0x00ff;
    state.sse.xmm[1] = 0xff00;

    state.control.cr0 |= CR0_TS;
    assert_eq!(step(&mut state, &mut bus), Err(Exception::DeviceNotAvailable));

    state.control.cr0 &= !CR0_TS;
    assert_eq!(step(&mut state, &mut bus), Ok(StepExit::Continue));
    assert_eq!(state.sse.xmm[0], 0x00ff ^ 0xff00);
}

#[test]
fn wait_with_ts_and_mp_raises_nm() {
    // wait/fwait
    let code = [0x9B];
    let (mut state, mut bus) = init(&code);
    state.control.cr0 |= CR0_TS | CR0_MP;
    assert_eq!(step(&mut state, &mut bus), Err(Exception::DeviceNotAvailable));
}

#[test]
fn wait_pending_exception_with_ne_raises_mf() {
    let code = [0x9B];
    let (mut state, mut bus) = init(&code);

    // Unmask invalid operation (IM=0) and set the corresponding status flag (IE=1).
    state.fpu.fcw = 0x037E;
    state.fpu.fsw = 0x0001;
    state.control.cr0 |= CR0_NE;

    assert_eq!(step(&mut state, &mut bus), Err(Exception::X87Fpu));
}

#[test]
fn x87_em_raises_ud() {
    // fninit
    let code = [0xDB, 0xE3];
    let (mut state, mut bus) = init(&code);
    state.control.cr0 |= CR0_EM;
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));
}

#[test]
fn x87_ts_raises_nm() {
    // fninit
    let code = [0xDB, 0xE3];
    let (mut state, mut bus) = init(&code);
    state.control.cr0 |= CR0_TS;
    assert_eq!(step(&mut state, &mut bus), Err(Exception::DeviceNotAvailable));
}

#[test]
fn sse_requires_osfxsr() {
    // xorps xmm0, xmm0
    let code = [0x0F, 0x57, 0xC0];
    let (mut state, mut bus) = init(&code);
    // Ensure `#UD` has priority over `#NM` when both OSFXSR=0 and TS=1.
    state.control.cr0 |= CR0_TS;
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));
}

#[test]
fn wait_pending_exception_with_ne0_sets_irq13_pending() {
    let code = [0x9B];
    let (mut state, mut bus) = init(&code);

    state.fpu.fcw = 0x037E;
    state.fpu.fsw = 0x0001;

    assert_eq!(step(&mut state, &mut bus), Ok(StepExit::Continue));
    assert!(state.irq13_pending());
}

#[test]
fn clts_clears_ts_and_allows_x87_execution() {
    // clts; fld1
    let code = [0x0F, 0x06, 0xD9, 0xE8];
    let (mut state, mut bus) = init(&code);
    state.control.cr0 |= CR0_TS;

    assert_eq!(step(&mut state, &mut bus), Ok(StepExit::Continue));
    assert_eq!(state.control.cr0 & CR0_TS, 0);

    assert_eq!(step(&mut state, &mut bus), Ok(StepExit::Continue));
}

#[test]
fn clts_requires_cpl0() {
    let code = [0x0F, 0x06];
    let (mut state, mut bus) = init(&code);
    state.control.cr0 |= CR0_TS;
    state.segments.cs.selector = 0x1B; // RPL3

    assert_eq!(step(&mut state, &mut bus), Err(Exception::gp0()));
    assert_ne!(state.control.cr0 & CR0_TS, 0);
}
