use aero_cpu_core::interp::tier0::exec::{step, StepExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::Exception;

#[test]
fn tier0_hlt_requires_cpl0() {
    const CODE_BASE: u64 = 0x1000;
    let mut bus = FlatTestBus::new(0x2000);
    bus.load(CODE_BASE, &[0xF4]); // HLT

    // CPL3 should fault with #GP(0).
    let mut user = CpuState::new(CpuMode::Bit32);
    user.set_rip(CODE_BASE);
    user.segments.cs.selector = 0x1B; // RPL3
    let err = step(&mut user, &mut bus).unwrap_err();
    assert_eq!(err, Exception::gp0());

    // CPL0 should halt.
    let mut kernel = CpuState::new(CpuMode::Bit32);
    kernel.set_rip(CODE_BASE);
    kernel.segments.cs.selector = 0x08; // RPL0
    let exit = step(&mut kernel, &mut bus).expect("step");
    assert_eq!(exit, StepExit::Halted);
    assert!(kernel.halted);
    // RIP should advance past the HLT instruction.
    assert_eq!(kernel.rip(), CODE_BASE + 1);
}
