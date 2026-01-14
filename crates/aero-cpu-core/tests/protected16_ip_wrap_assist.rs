use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, SEG_ACCESS_DB};
use aero_cpu_core::CpuCore;

#[test]
fn tier0_assist_runner_wraps_across_16bit_ip_boundary_in_protected16() {
    // Regression test for 16-bit protected-mode code segments (CS.D=0):
    // - IP is 16-bit and wraps at 0xFFFF.
    // - Tier-0's assist runner (`run_batch_with_assists`) should treat fallthrough across the wrap
    //   boundary as normal sequential execution (not as a control-flow boundary).
    //
    // Arrange `CPUID` (0F A2) across the IP wrap boundary, followed by `HLT` at IP=0x0001.
    // If the runner computes `next_ip` using the coarse `CpuMode::Protected` mask (0xFFFF_FFFF)
    // instead of the effective 16-bit IP mask, it will incorrectly treat the assist as a branch
    // boundary and return `BatchExit::Branch` before executing `HLT`.
    let mut bus = FlatTestBus::new(0x11_000);
    bus.write_u8(0xFFFF, 0x0F).unwrap();
    bus.write_u8(0x0000, 0xA2).unwrap();
    bus.write_u8(0x0001, 0xF4).unwrap();

    let mut cpu = CpuCore::new(CpuMode::Protected);
    // Force a 16-bit code segment in protected mode (CS.D=0).
    cpu.state.segments.cs.access &= !SEG_ACCESS_DB;
    cpu.state.segments.cs.base = 0;
    cpu.state.set_rip(0xFFFF);
    assert_eq!(cpu.state.bitness(), 16);

    let mut ctx = AssistContext::default();
    let res = run_batch_with_assists(&mut ctx, &mut cpu, &mut bus, 16);
    assert_eq!(res.exit, BatchExit::Halted, "batch should run through HLT");
    assert!(cpu.state.halted);
    assert_eq!(cpu.state.rip(), 0x0002);
    assert_eq!(res.executed, 2);
}
