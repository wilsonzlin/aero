use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_serial_boot_sector(message: &[u8]) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov dx, 0x3f8
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
    i += 3;

    for &b in message {
        // mov al, imm8
        sector[i..i + 2].copy_from_slice(&[0xB0, b]);
        i += 2;
        // out dx, al
        sector[i] = 0xEE;
        i += 1;
    }

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => break,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
}

#[test]
fn reset_is_idempotent_for_bios_rom_mappings() {
    // Regression test: `Machine::reset()` may be called multiple times (e.g. tests that swap the
    // disk image). BIOS POST always attempts to map the BIOS ROM windows, and the platform memory
    // bus treats overlaps as errors. Ensure repeated resets are safe and don't panic.
    let mut m = Machine::new(MachineConfig {
        // Use a small-but-realistic RAM size so BIOS POST can place ACPI/SMBIOS structures without
        // risking out-of-range writes.
        ram_size_bytes: 16 * 1024 * 1024,
        // Enable the canonical PC platform wiring so `reset()` exercises both:
        // - BIOS ROM re-mapping, and
        // - MMIO window mapping (LAPIC/IOAPIC/HPET/PCI ECAM, etc.).
        enable_pc_platform: true,
        ..Default::default()
    })
    .unwrap();

    let boot = build_serial_boot_sector(b"OK\n");
    m.set_disk_image(boot.to_vec()).unwrap();

    // Repeated resets should be safe (no ROM/MMIO overlap panics).
    for _ in 0..3 {
        m.reset();
    }

    run_until_halt(&mut m);
    assert_eq!(m.take_serial_output(), b"OK\n");
}
