use aero_cpu_core::exec::{Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{gpr, CpuMode};

#[cfg(debug_assertions)]
#[test]
fn tier0_decode_cache_hits_on_reexecution() {
    let code_base = 0x1000u64;
    let mut bus = FlatTestBus::new(0x10000);
    // NOP; NOP; NOP
    bus.load(code_base, &[0x90, 0x90, 0x90]);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Real, bus);
    cpu.cpu.state.segments.cs.selector = 0;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(code_base);

    let mut interp = Tier0Interpreter::new(3);

    interp.exec_block(&mut cpu);
    let stats_after_first = interp.decode_cache_stats();
    assert_eq!(stats_after_first.hits, 0);
    assert_eq!(stats_after_first.misses, 3);

    cpu.cpu.state.set_rip(code_base);
    interp.exec_block(&mut cpu);

    let stats_after_second = interp.decode_cache_stats();
    assert_eq!(stats_after_second.hits, 3);
    assert_eq!(stats_after_second.misses, 3);
}

#[cfg(debug_assertions)]
#[test]
fn tier0_decode_cache_invalidated_when_code_bytes_change() {
    let code_base = 0x2000u64;
    let mut bus = FlatTestBus::new(0x10000);
    // mov ax, 1
    bus.load(code_base, &[0xB8, 0x01, 0x00]);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Real, bus);
    cpu.cpu.state.segments.cs.selector = 0;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(code_base);

    let mut interp = Tier0Interpreter::new(1);

    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.read_gpr16(gpr::RAX), 1);
    let stats_after_first = interp.decode_cache_stats();
    assert_eq!(stats_after_first.hits, 0);
    assert_eq!(stats_after_first.misses, 1);

    // Patch the code: mov ax, 2
    cpu.bus.load(code_base, &[0xB8, 0x02, 0x00]);
    cpu.cpu.state.set_rip(code_base);

    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.read_gpr16(gpr::RAX), 2);

    let stats_after_second = interp.decode_cache_stats();
    assert_eq!(stats_after_second.hits, 0);
    assert_eq!(stats_after_second.misses, 2);
}
