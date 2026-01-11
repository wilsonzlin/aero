use std::io::Cursor;

use emulator::smp::{Machine, VcpuRunState, APIC_REG_ICR_HIGH, APIC_REG_ICR_LOW, RESET_VECTOR};

fn icr_high_dest(apic_id: u8) -> u32 {
    (apic_id as u32) << 24
}

fn icr_low_init_assert() -> u32 {
    // Delivery mode = INIT (5), level = assert.
    (5u32 << 8) | (1u32 << 14)
}

fn icr_low_sipi(vector: u8) -> u32 {
    // Delivery mode = STARTUP (6).
    (vector as u32) | (6u32 << 8)
}

fn icr_low_fixed(vector: u8) -> u32 {
    // Delivery mode = FIXED (0).
    vector as u32
}

fn icr_low_fixed_shorthand_all_excluding_self(vector: u8) -> u32 {
    // Delivery mode = FIXED (0), destination shorthand = all-excluding-self (3).
    (vector as u32) | (3u32 << 18)
}

#[test]
fn smp_snapshot_roundtrip_preserves_cpu_apic_and_ram() {
    let cpu_count = 4;
    let mem_size = 0x40000;
    let mut machine = Machine::new(cpu_count, mem_size);

    // Install an AP trampoline and start a couple of APs.
    let tramp = machine
        .install_trampoline(0x8000, &[0xF4, 0xF4, 0xF4])
        .unwrap();

    for apic_id in [1u8, 2u8] {
        machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(apic_id));
        machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_init_assert());
        machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(apic_id));
        machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_sipi(tramp.vector));
    }

    // Queue a few IPIs (multiple per target to validate ordering).
    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
    machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_fixed(0x40));

    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(2));
    machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_fixed(0x41));

    machine.write_local_apic(1, APIC_REG_ICR_HIGH, icr_high_dest(0));
    machine.write_local_apic(
        1,
        APIC_REG_ICR_LOW,
        icr_low_fixed_shorthand_all_excluding_self(0x42),
    );

    machine.write_local_apic(2, APIC_REG_ICR_HIGH, icr_high_dest(1));
    machine.write_local_apic(2, APIC_REG_ICR_LOW, icr_low_fixed(0x43));

    // Mutate memory in a few places (including around the trampoline).
    machine.write_memory(0x1000, &[0xAA, 0xBB, 0xCC]).unwrap();
    machine
        .write_memory(0x7FF0, &[0xDE, 0xAD, 0xBE, 0xEF])
        .unwrap();
    machine.write_memory(0x8000, &[0x90, 0x90]).unwrap();

    // Also leave some ICR_HIGH state set without writing ICR_LOW.
    machine.write_local_apic(3, APIC_REG_ICR_HIGH, icr_high_dest(2));

    assert_eq!(machine.cpus[0].cpu.run_state, VcpuRunState::Running);
    assert_eq!(machine.cpus[0].cpu.rip, RESET_VECTOR);
    assert_eq!(machine.cpus[1].cpu.run_state, VcpuRunState::Running);
    assert_eq!(machine.cpus[1].cpu.rip, 0x8000);
    assert_eq!(machine.cpus[2].cpu.run_state, VcpuRunState::Running);
    assert_eq!(machine.cpus[2].cpu.rip, 0x8000);
    assert_eq!(machine.cpus[3].cpu.run_state, VcpuRunState::WaitForSipi);

    let mut out = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(
        &mut out,
        &mut machine,
        aero_snapshot::SaveOptions::default(),
    )
    .unwrap();

    let bytes = out.into_inner();

    let mut restored = Machine::new(cpu_count, mem_size);
    aero_snapshot::restore_snapshot(&mut Cursor::new(&bytes), &mut restored).unwrap();

    assert_eq!(restored.trampoline, machine.trampoline);
    assert_eq!(restored.memory, machine.memory);

    for i in 0..cpu_count {
        let orig = &machine.cpus[i];
        let new = &restored.cpus[i];
        assert_eq!(new.cpu.apic_id, orig.cpu.apic_id);
        assert_eq!(new.cpu.is_bsp, orig.cpu.is_bsp);
        assert_eq!(new.cpu.rip, orig.cpu.rip);
        assert_eq!(new.cpu.run_state, orig.cpu.run_state);
        assert_eq!(new.cpu.sipi_vector, orig.cpu.sipi_vector);

        assert_eq!(
            restored.read_local_apic(i, APIC_REG_ICR_HIGH),
            machine.read_local_apic(i, APIC_REG_ICR_HIGH),
            "ICR_HIGH mismatch for cpu {i}"
        );
    }

    // Compare pending interrupt queues by draining them in lockstep.
    for cpu in 0..cpu_count {
        loop {
            let orig = machine.pop_pending_interrupt(cpu);
            let new = restored.pop_pending_interrupt(cpu);
            assert_eq!(new, orig, "pending interrupt mismatch for cpu {cpu}");
            if orig.is_none() {
                break;
            }
        }
    }
}
