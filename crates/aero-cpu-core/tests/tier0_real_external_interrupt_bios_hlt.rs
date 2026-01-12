use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, RFLAGS_IF, RFLAGS_RESERVED1};
use aero_cpu_core::{CpuBus, CpuCore};
use aero_x86::Register;

fn write_ivt_entry(bus: &mut FlatTestBus, vector: u8, offset: u16, segment: u16) {
    let addr = (vector as u64) * 4;
    bus.write_u16(addr, offset).unwrap();
    bus.write_u16(addr + 2, segment).unwrap();
}

#[test]
fn tier0_external_interrupt_sets_bios_vector_for_hlt_stub_in_real_mode() {
    let mut bus = FlatTestBus::new(0x10000);

    // IVT[0x08] -> 0000:0500
    let vector = 0x08u8;
    let handler_seg = 0x0000u16;
    let handler_off = 0x0500u16;
    write_ivt_entry(&mut bus, vector, handler_off, handler_seg);

    // Handler: HLT; IRET
    let handler_phys = ((handler_seg as u64) << 4) + handler_off as u64;
    bus.load(handler_phys, &[0xF4, 0xCF]);

    // Some placeholder code for the interrupted context (should never run).
    bus.load(0x0100, &[0xF4]); // HLT

    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.state.write_reg(Register::CS, 0);
    cpu.state.write_reg(Register::DS, 0);
    cpu.state.write_reg(Register::SS, 0);
    cpu.state.write_reg(Register::SP, 0x8000);
    cpu.state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);
    cpu.state.set_rip(0x0100);

    // Deliver a maskable external interrupt (e.g. PIT IRQ0). In real mode many BIOS IVT entries
    // point to ROM stubs that begin with `HLT; IRET`.
    cpu.pending.inject_external_interrupt(vector);

    let mut ctx = AssistContext::default();
    let cfg = Tier0Config::default();
    let res = run_batch_cpu_core_with_assists(&cfg, &mut ctx, &mut cpu, &mut bus, 16);

    assert_eq!(res.exit, BatchExit::BiosInterrupt(vector));
}
