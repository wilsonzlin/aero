mod common;

use std::ops::Range;

use firmware::bus::Bus;
use firmware::devices::{Hpet, Pit};
use firmware::e820::E820Entry;
use firmware::realmode::RealModeCpu;
use firmware::validate::{validate_acpi, validate_e820};

use common::TestMachine;

#[test]
fn acpi_tables_have_valid_checksums_and_structure() {
    let mut m = TestMachine::new();
    validate_acpi(
        &mut m.bus,
        m.bios.acpi.rsdp_address as u32,
        common::DEFAULT_HPET_BASE,
    )
    .unwrap();
}

#[test]
fn e820_map_has_no_overlaps_and_respects_reserved_regions() {
    let m = TestMachine::new();
    let reserved: [Range<u64>; 1] = [0x0009_FC00..0x0010_0000];
    validate_e820(&m.bios.e820, &reserved).unwrap();

    // Sanity: the reserved region must not be marked as RAM.
    for entry in &m.bios.e820 {
        if entry.typ != E820Entry::TYPE_RAM {
            continue;
        }
        assert!(
            entry.end() <= reserved[0].start || entry.base >= reserved[0].end,
            "RAM entry overlaps reserved BIOS/VGA region: {:?}",
            entry
        );
    }
}

#[test]
fn int10_teletype_writes_to_serial_buffer() {
    let mut m = TestMachine::new();
    m.bus.clear_serial();

    let mut cpu = RealModeCpu::default();
    cpu.set_ah(0x0E);
    cpu.set_al(b'X');
    m.bios.handle_int10(&mut m.bus, &mut cpu);

    assert_eq!(m.bus.serial_output(), b"X");
    assert!(!cpu.carry(), "INT10 teletype should clear CF");
}

#[test]
fn int16_read_key_returns_scancode_and_ascii() {
    let mut m = TestMachine::new();
    m.bios.keyboard.push_key(b'A', 0x1E);

    let mut cpu = RealModeCpu::default();
    cpu.set_ah(0x00);
    m.bios.handle_int16(&mut m.bus, &mut cpu);

    assert_eq!(cpu.ax(), 0x1E41);
    assert!(!cpu.carry());
}

#[test]
fn int13_read_sector_reads_into_memory() {
    // Two sectors: boot sector (ignored) + one data sector we will read.
    let mut disk = vec![0u8; 2 * 512];
    disk[512] = 0x42;

    let mut m = TestMachine::new().with_disk(disk);

    let mut cpu = RealModeCpu::default();
    cpu.es = 0x0000;
    cpu.set_bx(0x0500);

    // AH=0x02 read, AL=1 sector.
    cpu.set_ax(0x0201);
    // CH=0 cylinder, CL=2 sector (boot is sector 1).
    cpu.set_cx(0x0002);
    // DH=0 head, DL=0x80 hard disk.
    cpu.set_dx(0x0080);

    m.bios.handle_int13(&mut m.bus, &mut cpu);

    assert!(!cpu.carry(), "INT13 read should clear CF");
    assert_eq!(cpu.ah(), 0);
    assert_eq!(m.bus.read_u8(0x0500), 0x42);
}

#[test]
fn int15_e820_writes_entry_to_memory_and_returns_continuation() {
    let mut m = TestMachine::new();

    let mut cpu = RealModeCpu::default();
    cpu.eax = 0xE820;
    cpu.edx = 0x534D_4150; // 'SMAP'
    cpu.ecx = 24;
    cpu.ebx = 0;
    cpu.es = 0;
    cpu.edi = 0x0600;

    m.bios.handle_int15(&mut m.bus, &mut cpu);

    assert!(!cpu.carry());
    assert_eq!(cpu.eax, 0x534D_4150);
    assert!(cpu.ecx == 24 || cpu.ecx == 20);

    // Base address should be 0 for the first default entry.
    let base = m.bus.read_u64(0x0600);
    assert_eq!(base, 0);

    // Continuation should either be 0 (single-entry map) or non-zero.
    assert!(
        cpu.ebx == 0 || cpu.ebx as usize == 1,
        "unexpected continuation value {}",
        cpu.ebx
    );
}

#[test]
fn timers_advance_deterministically_without_wall_clock() {
    let mut m = TestMachine::new();

    let pit_before = m.bus.devices.pit.ticks();
    m.bus.devices.pit.advance_ns(1_000_000_000);
    assert_eq!(m.bus.devices.pit.ticks() - pit_before, Pit::HZ);

    m.bus.devices.hpet.advance_ns(1_000_000_000);
    assert_eq!(m.bus.devices.hpet.counter(), Hpet::HZ);

    // HPET main counter is exposed via MMIO at base+0xF0.
    let mmio_counter = m.bus.read_u64(common::DEFAULT_HPET_BASE as u32 + 0xF0);
    assert_eq!(mmio_counter, Hpet::HZ);
}
