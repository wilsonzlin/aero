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
fn x87_em_has_priority_over_ts() {
    // fld1
    let code = [0xD9, 0xE8];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_EM | CR0_TS;
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
fn wait_em_has_priority_over_nm() {
    // wait/fwait
    let code = [0x9B];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_EM | CR0_MP | CR0_TS;
    assert_eq!(exec_single(&code, &mut state), Err(Exception::InvalidOpcode));
}

#[test]
fn wait_ts_without_mp_does_not_raise_nm() {
    // wait/fwait
    let code = [0x9B];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_TS;
    assert_eq!(exec_single(&code, &mut state), Ok(StepExit::Continue));
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

#[test]
fn wait_with_pending_unmasked_exception_ts_without_mp_still_raises_mf() {
    // wait/fwait
    let code = [0x9B];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_TS | CR0_NE;
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation
    assert_eq!(exec_single(&code, &mut state), Err(Exception::X87Fpu));
}

#[test]
fn finit_waits_and_raises_mf() {
    // finit (wait + fninit)
    let code = [0x9B, 0xDB, 0xE3];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_NE;
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation
    assert_eq!(exec_single(&code, &mut state), Err(Exception::X87Fpu));
    // Should fault before executing FNINIT, so the pending exception is still present.
    assert_eq!(state.fpu.fsw & 0x3F, 0x0001);
}

#[test]
fn fstsw_wait_form_raises_mf_before_writing_ax() {
    // fstsw ax (wait + fnstsw ax)
    let code = [0x9B, 0xDF, 0xE0];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_NE;
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation
    state.write_reg(Register::AX, 0xBEEF);
    assert_eq!(exec_single(&code, &mut state), Err(Exception::X87Fpu));
    assert_eq!(state.read_reg(Register::AX), 0xBEEF);
}

#[test]
fn fninit_no_wait_clears_pending_exception_without_mf() {
    // fninit (no-wait form)
    let code = [0xDB, 0xE3];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_NE;
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation

    assert_eq!(exec_single(&code, &mut state), Ok(StepExit::Continue));
    assert_eq!(state.fpu.fsw & 0x3F, 0); // exception flags cleared
    assert_eq!(state.fpu.fcw, 0x037F); // reset to default control word
}

#[test]
fn fnclex_no_wait_clears_pending_exception_without_mf() {
    // fnclex (no-wait form)
    let code = [0xDB, 0xE2];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_NE;
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation

    assert_eq!(exec_single(&code, &mut state), Ok(StepExit::Continue));
    assert_eq!(state.fpu.fsw & 0x3F, 0); // exception flags cleared
    assert_eq!(state.fpu.fcw, 0x037E); // control word unchanged
}

#[test]
fn fnstsw_no_wait_does_not_raise_mf_and_writes_ax() {
    // fnstsw ax (no-wait form)
    let code = [0xDF, 0xE0];
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_NE;
    state.fpu.fcw = 0x037E; // unmask invalid operation
    state.fpu.fsw = 0x0001; // pending invalid operation
    state.write_reg(Register::AX, 0xBEEF);

    assert_eq!(exec_single(&code, &mut state), Ok(StepExit::Continue));
    // Status word should have been written and contain the pending exception.
    let ax = state.read_reg(Register::AX) as u16;
    assert_ne!(ax, 0xBEEF);
    assert_ne!(ax & 0x3F, 0);
}
