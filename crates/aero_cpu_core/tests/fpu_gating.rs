use aero_cpu_core::system::{Cpu, CR0_EM, CR0_MP, CR0_TS, CR4_OSFXSR};
use aero_cpu_core::{CpuState, Exception, FXSAVE_AREA_SIZE};

#[test]
fn sse_ts_raises_nm_and_clearing_ts_allows_execution() {
    let mut cpu = Cpu::default();
    cpu.cs = 0x8; // CPL0
    cpu.cr4 |= CR4_OSFXSR;

    let mut state = CpuState::default();
    state.sse.xmm[0] = 0x00ff;
    state.sse.xmm[1] = 0xff00;

    cpu.cr0 |= CR0_TS;
    assert_eq!(
        cpu.instr_xorps(&mut state, 0, 1),
        Err(Exception::DeviceNotAvailable)
    );

    cpu.cr0 &= !CR0_TS;
    assert_eq!(cpu.instr_xorps(&mut state, 0, 1), Ok(()));
    assert_eq!(state.sse.xmm[0], 0x00ff ^ 0xff00);
}

#[test]
fn wait_with_ts_and_mp_raises_nm() {
    let mut cpu = Cpu::default();
    cpu.cr0 |= CR0_TS | CR0_MP;

    let state = CpuState::default();
    assert_eq!(
        cpu.instr_wait(&state.fpu),
        Err(Exception::DeviceNotAvailable)
    );
}

#[test]
fn sse_requires_osfxsr() {
    let mut cpu = Cpu::default();
    let mut state = CpuState::default();

    assert_eq!(cpu.instr_xorps(&mut state, 0, 0), Err(Exception::InvalidOpcode));

    let mut fx_area = [0u8; FXSAVE_AREA_SIZE];
    assert_eq!(
        cpu.instr_fxsave(&state, &mut fx_area),
        Err(Exception::InvalidOpcode)
    );
}

#[test]
fn x87_em_raises_ud() {
    let mut cpu = Cpu::default();
    cpu.cr0 |= CR0_EM;

    let mut state = CpuState::default();
    assert_eq!(cpu.instr_fninit(&mut state), Err(Exception::InvalidOpcode));
}

#[test]
fn clts_clears_ts_and_unblocks_sse() {
    let mut cpu = Cpu::default();
    cpu.cs = 0x8; // CPL0
    cpu.cr4 |= CR4_OSFXSR;
    cpu.cr0 |= CR0_TS;

    let mut state = CpuState::default();
    assert_eq!(
        cpu.instr_xorps(&mut state, 0, 0),
        Err(Exception::DeviceNotAvailable)
    );

    cpu.clts().unwrap();
    assert_eq!(cpu.instr_xorps(&mut state, 0, 0), Ok(()));
}

