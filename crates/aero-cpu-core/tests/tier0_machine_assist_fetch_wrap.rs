mod common;

use aero_cpu_core::state::{CpuMode, CpuState};
use aero_x86::Register;
use common::machine::{TestBus, Tier0Machine};

#[test]
fn tier0_machine_assist_fetch_wraps_across_16bit_ip_boundary() {
    // `OUT imm8, AL` is handled via an assist in Tier-0. The Tier0Machine test harness decodes the
    // faulting instruction itself, so its instruction fetch must obey 16-bit IP wrapping.
    //
    // Place the opcode at IP=0xFFFF and the immediate port byte at IP=0x0000.
    //
    // If the assist fetch does *not* wrap, it would incorrectly read the port byte from 0x10000
    // instead and fail to write to the debugcon port (0xE9).
    let mut bus = TestBus::new(0x20000);
    bus.load(0xFFFF, &[0xE6]); // out imm8, al
    bus.load(0x0000, &[0xE9, 0xF4]); // port=0xE9; hlt
    bus.load(0x1_0000, &[0x00]); // sentinel port if wrapping is broken

    let mut cpu = CpuState::new(CpuMode::Bit16);
    cpu.write_reg(Register::CS, 0);
    cpu.set_rip(0xFFFF);
    cpu.write_reg(Register::AL, b'X' as u64);

    let mut machine = Tier0Machine::new(cpu, bus);
    machine.run(64);

    assert_eq!(machine.bus.debugcon(), b"X");
}

