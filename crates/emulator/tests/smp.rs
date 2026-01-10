use emulator::smp::{
    DeterministicScheduler, Guest, Machine, VcpuRunState, APIC_REG_ICR_HIGH, APIC_REG_ICR_LOW,
    RESET_VECTOR,
};
use firmware::acpi::{AcpiConfig, AcpiTables};

fn madt_processor_apic_ids(madt: &[u8]) -> Vec<u8> {
    // MADT = ACPI header (36) + LAPIC addr (4) + flags (4) + entries.
    if madt.len() < 44 || &madt[0..4] != b"APIC" {
        return Vec::new();
    }

    let total_len = u32::from_le_bytes(madt[4..8].try_into().unwrap()) as usize;
    if total_len > madt.len() || total_len < 44 {
        return Vec::new();
    }

    let mut apic_ids = Vec::new();
    let mut offset = 44;
    while offset + 2 <= total_len {
        let entry_type = madt[offset];
        let entry_len = madt[offset + 1] as usize;
        if entry_len < 2 || offset + entry_len > total_len {
            break;
        }

        // Processor Local APIC (type 0): [type, len, acpi_id, apic_id, flags u32]
        if entry_type == 0 && entry_len >= 8 {
            apic_ids.push(madt[offset + 3]);
        }

        offset += entry_len;
    }

    apic_ids
}

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
fn ap_bring_up_init_sipi() {
    let mut machine = Machine::new(2, 0x20000);

    // BSP starts at the reset vector; APs begin in wait-for-SIPI.
    assert_eq!(machine.cpus[0].cpu.rip, RESET_VECTOR);
    assert_eq!(machine.cpus[0].cpu.run_state, VcpuRunState::Running);
    assert_eq!(machine.cpus[1].cpu.run_state, VcpuRunState::WaitForSipi);

    // BIOS would install the AP trampoline and then send INIT+SIPI.
    let tramp = machine.install_trampoline(0x8000, &[0xF4]).unwrap();

    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
    machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_init_assert());

    assert_eq!(machine.cpus[1].cpu.run_state, VcpuRunState::WaitForSipi);
    assert_eq!(machine.cpus[1].cpu.rip, 0);

    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
    machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_sipi(tramp.vector));

    assert_eq!(machine.cpus[1].cpu.run_state, VcpuRunState::Running);
    assert_eq!(machine.cpus[1].cpu.rip, 0x8000);
    assert_eq!(machine.cpus[1].cpu.sipi_vector, Some(tramp.vector));
}

#[test]
fn fixed_ipi_is_delivered_to_target_cpu() {
    let mut machine = Machine::new(2, 0x20000);
    let tramp = machine.install_trampoline(0x8000, &[0xF4]).unwrap();

    // Bring up AP.
    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
    machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_init_assert());
    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
    machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_sipi(tramp.vector));

    // Send a fixed IPI.
    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
    machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_fixed(0x40));

    assert_eq!(machine.pop_pending_interrupt(1), Some(0x40));
    assert_eq!(machine.pop_pending_interrupt(1), None);
}

#[test]
fn ipi_destination_shorthand_all_excluding_self() {
    let mut machine = Machine::new(3, 0x20000);

    machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(0));
    machine.write_local_apic(
        0,
        APIC_REG_ICR_LOW,
        icr_low_fixed_shorthand_all_excluding_self(0x41),
    );

    assert_eq!(machine.pop_pending_interrupt(0), None);
    assert_eq!(machine.pop_pending_interrupt(1), Some(0x41));
    assert_eq!(machine.pop_pending_interrupt(2), Some(0x41));
}

#[test]
fn madt_enumerates_multiple_processors() {
    let cfg = AcpiConfig::new(3, 16 * 1024 * 1024);
    let tables = AcpiTables::build(&cfg).unwrap();
    let ids = madt_processor_apic_ids(&tables.madt);
    assert_eq!(ids, vec![0, 1, 2]);
}

#[test]
fn synthetic_smp_guest_boots_and_receives_ipi() {
    struct SmpGuest {
        booted: [bool; 2],
        ipi_received: [u32; 2],
        sent_startup: bool,
        sent_ipi: bool,
        sipi_vector: u8,
    }

    impl Guest for SmpGuest {
        fn on_tick(&mut self, cpu: usize, machine: &mut Machine) {
            self.booted[cpu] = machine.cpus[cpu].cpu.run_state == VcpuRunState::Running;

            if cpu == 0 && !self.sent_startup {
                machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
                machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_init_assert());
                machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
                machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_sipi(self.sipi_vector));
                self.sent_startup = true;
            }

            if cpu == 0
                && self.sent_startup
                && !self.sent_ipi
                && machine.cpus[1].cpu.run_state == VcpuRunState::Running
            {
                machine.write_local_apic(0, APIC_REG_ICR_HIGH, icr_high_dest(1));
                machine.write_local_apic(0, APIC_REG_ICR_LOW, icr_low_fixed(0x40));
                self.sent_ipi = true;
            }
        }

        fn on_interrupt(&mut self, cpu: usize, vector: u8, _machine: &mut Machine) {
            if vector == 0x40 {
                self.ipi_received[cpu] += 1;
            }
        }
    }

    let mut machine = Machine::new(2, 0x20000);
    let tramp = machine.install_trampoline(0x8000, &[0xF4]).unwrap();

    // "Guest OS sees 2 CPUs": read MADT.
    let cfg = AcpiConfig::new(machine.cpus.len() as u8, 16 * 1024 * 1024);
    let tables = AcpiTables::build(&cfg).unwrap();
    let ids = madt_processor_apic_ids(&tables.madt);
    assert_eq!(ids, vec![0, 1]);

    let mut guest = SmpGuest {
        booted: [false; 2],
        ipi_received: [0; 2],
        sent_startup: false,
        sent_ipi: false,
        sipi_vector: tramp.vector,
    };

    let mut sched = DeterministicScheduler::new();
    sched.run_for_ticks(&mut machine, &mut guest, 64);

    assert!(guest.booted[0], "BSP should be running");
    assert!(guest.booted[1], "AP should have been started via SIPI");
    assert_eq!(guest.ipi_received[1], 1, "AP should receive the fixed IPI");
}
