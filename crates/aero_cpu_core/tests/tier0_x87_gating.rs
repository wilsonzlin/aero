use aero_cpu_core::interp::tier0::exec::{step, StepExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_EM, CR0_MP, CR0_NE, CR0_TS};
use aero_cpu_core::Exception;
use aero_x86::Register;

fn exec_single(code: &[u8], state: &mut CpuState) -> Result<StepExit, Exception> {
    let mut bus = FlatTestBus::new(0x1000);
    bus.load(0, code);
    // Ensure CS base is 0 so RIP is a flat linear address.
    state.segments.cs.base = 0;
    state.write_reg(Register::CS, 0);
    state.set_rip(0);
    step(state, &mut bus)
}

#[test]
fn x87_ts_raises_nm() {
    // fld1
    let code = [0xD9, 0xE8];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_TS;
    assert_eq!(
        exec_single(&code, &mut state),
        Err(Exception::DeviceNotAvailable)
    );
}

#[test]
fn x87_em_raises_ud() {
    // fld1
    let code = [0xD9, 0xE8];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_EM;
    assert_eq!(exec_single(&code, &mut state), Err(Exception::InvalidOpcode));
}

#[test]
fn wait_mp_ts_raises_nm() {
    // wait/fwait
    let code = [0x9B];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_MP | CR0_TS;
    assert_eq!(
        exec_single(&code, &mut state),
        Err(Exception::DeviceNotAvailable)
    );
}

#[test]
fn wait_with_pending_unmasked_exception_ne_raises_mf() {
    // wait/fwait
    let code = [0x9B];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_NE;
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation
    assert_eq!(exec_single(&code, &mut state), Err(Exception::X87Fpu));
}

#[test]
fn wait_with_pending_unmasked_exception_ne0_sets_irq13_pending() {
    // wait/fwait
    let code = [0x9B];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation
    assert_eq!(exec_single(&code, &mut state), Ok(StepExit::Continue));
    assert!(state.irq13_pending);
}

