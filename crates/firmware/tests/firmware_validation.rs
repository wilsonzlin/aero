mod common;

use std::ops::Range;

use firmware::bus::Bus;
use firmware::devices::{Hpet, Pit};
use firmware::e820::E820Entry;
use firmware::realmode::RealModeCpu;
use firmware::validate::{validate_acpi, validate_e820};
use firmware::{bus::TestBus, devices::Devices, legacy_bios::LegacyBios};

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
fn int15_a20_toggle_affects_address_masking_in_bus() {
    let mut m = TestMachine::new();

    // Disable A20 via INT 15h AX=2400.
    let mut cpu = RealModeCpu::default();
    cpu.set_ax(0x2400);
    m.bios.handle_int15(&mut m.bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.ah(), 0);

    // With A20 disabled, 0x1_00000 aliases to 0x0.
    m.bus.write_u8(0x0, 0xAA);
    assert_eq!(m.bus.read_u8(0x1_00000), 0xAA);

    // Enable A20 via INT 15h AX=2401.
    cpu.set_ax(0x2401);
    m.bios.handle_int15(&mut m.bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.ah(), 0);

    m.bus.write_u8(0x1_00000, 0xBB);
    assert_eq!(m.bus.read_u8(0x0), 0xAA);
    assert_eq!(m.bus.read_u8(0x1_00000), 0xBB);
}

#[test]
fn int15_a20_query_reports_bus_state() {
    let mut m = TestMachine::new();
    let mut cpu = RealModeCpu::default();

    // Default TestBus starts with A20 enabled to avoid breaking MMIO accesses in
    // other firmware validation tests.
    cpu.set_ax(0x2402);
    m.bios.handle_int15(&mut m.bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.ah(), 0);
    assert_eq!(cpu.al(), 1);

    cpu.set_ax(0x2400);
    m.bios.handle_int15(&mut m.bus, &mut cpu);
    assert!(!cpu.carry());

    cpu.set_ax(0x2402);
    m.bios.handle_int15(&mut m.bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.al(), 0);
}

#[test]
fn int15_e801_reports_expected_sizes() {
    struct Case {
        ram_size_bytes: u64,
        ax_kb: u16,
        bx_blocks: u16,
    }

    let cases = [
        Case {
            ram_size_bytes: 512 * 1024 * 1024,
            ax_kb: 0x3C00,
            bx_blocks: 0x1F00,
        },
        Case {
            ram_size_bytes: 2 * 1024 * 1024 * 1024,
            ax_kb: 0x3C00,
            bx_blocks: 0x7F00,
        },
        Case {
            ram_size_bytes: 4 * 1024 * 1024 * 1024,
            ax_kb: 0x3C00,
            bx_blocks: 0xFF00,
        },
    ];

    for case in cases {
        let devices = Devices::new(common::DEFAULT_HPET_BASE);
        let mut bus = TestBus::new(2 * 1024 * 1024, devices);
        let mut bios = LegacyBios::new(firmware::legacy_bios::BiosConfig {
            ram_size: case.ram_size_bytes,
            acpi_base: common::DEFAULT_ACPI_BASE,
            hpet_base: common::DEFAULT_HPET_BASE,
        });
        bios.post(&mut bus);

        let mut cpu = RealModeCpu::default();
        cpu.eax = 0xE801;
        bios.handle_int15(&mut bus, &mut cpu);

        assert!(!cpu.carry(), "E801 should succeed for {}", case.ram_size_bytes);
        assert_eq!(cpu.ax(), case.ax_kb);
        assert_eq!(cpu.bx(), case.bx_blocks);
        assert_eq!(cpu.cx(), case.ax_kb);
        assert_eq!(cpu.dx(), case.bx_blocks);
    }
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
