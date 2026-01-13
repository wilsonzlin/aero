use std::collections::BTreeSet;

use aero_machine::{Machine, MachineConfig};
use firmware::acpi::LOCAL_APIC_BASE;

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

fn scan_region_for_rsdp(machine: &mut Machine, base: u64, len: usize) -> Option<u64> {
    let buf = machine.read_physical_bytes(base, len);
    for off in (0..len.saturating_sub(8)).step_by(16) {
        if &buf[off..off + 8] != b"RSD PTR " {
            continue;
        }

        // The signature matches. Confirm this is a real RSDP by validating checksum(s).
        let rsdp_addr = base + off as u64;
        let rsdp_v1 = machine.read_physical_bytes(rsdp_addr, 20);
        if checksum(&rsdp_v1) != 0 {
            continue;
        }
        let revision = rsdp_v1[15];
        if revision >= 2 {
            // ACPI 2.0+: validate the extended checksum too.
            let ext = machine.read_physical_bytes(rsdp_addr + 20, 16);
            let length = read_u32_le(&ext, 0) as usize;
            if length < 36 {
                continue;
            }
            let rsdp_full = machine.read_physical_bytes(rsdp_addr, length);
            if checksum(&rsdp_full) != 0 {
                continue;
            }
        }

        return Some(rsdp_addr);
    }
    None
}

fn find_rsdp(machine: &mut Machine) -> u64 {
    // ACPI spec: search first KiB of the EBDA, else scan 0xE0000-0xFFFFF on 16-byte boundaries.
    let ebda_seg = machine.read_physical_u16(0x40E);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u64) << 4;
        if let Some(addr) = scan_region_for_rsdp(machine, ebda_base, 1024) {
            return addr;
        }
    }

    scan_region_for_rsdp(machine, 0xE0000, 0x20000).expect("RSDP not found in EBDA or BIOS region")
}

fn find_madt(machine: &mut Machine, rsdp_addr: u64) -> u64 {
    // --- RSDP ---
    let rsdp_v1 = machine.read_physical_bytes(rsdp_addr, 20);
    assert_eq!(&rsdp_v1[0..8], b"RSD PTR ");
    assert_eq!(checksum(&rsdp_v1), 0, "RSDP v1 checksum");

    let revision = rsdp_v1[15];
    let rsdt_addr = u64::from(read_u32_le(&rsdp_v1, 16));
    let (sdt_addr, entry_size) = if revision >= 2 {
        let ext = machine.read_physical_bytes(rsdp_addr + 20, 16);
        let length = read_u32_le(&ext, 0) as usize;
        let rsdp_full = machine.read_physical_bytes(rsdp_addr, length);
        assert_eq!(checksum(&rsdp_full), 0, "RSDP extended checksum");
        let xsdt_addr = read_u64_le(&rsdp_full, 24);
        // Prefer XSDT when present (ACPI 2.0+), but allow fallback to RSDT.
        if xsdt_addr != 0 {
            (xsdt_addr, 8usize)
        } else {
            (rsdt_addr, 4usize)
        }
    } else {
        (rsdt_addr, 4usize)
    };

    // --- XSDT/RSDT ---
    let sdt_hdr = machine.read_physical_bytes(sdt_addr, 36);
    let signature = [sdt_hdr[0], sdt_hdr[1], sdt_hdr[2], sdt_hdr[3]];
    let length = read_u32_le(&sdt_hdr, 4) as usize;
    assert!(
        length >= 36,
        "SDT length too small: {length} (signature={:?})",
        std::str::from_utf8(&signature).unwrap_or("<non-ascii>")
    );

    let sdt = machine.read_physical_bytes(sdt_addr, length);
    assert_eq!(
        checksum(&sdt),
        0,
        "XSDT/RSDT checksum (signature={:?})",
        std::str::from_utf8(&signature).unwrap_or("<non-ascii>")
    );

    let entries_len = length - 36;
    assert!(
        entries_len.is_multiple_of(entry_size),
        "XSDT/RSDT entries size mismatch: entries_len={entries_len} entry_size={entry_size}"
    );
    let entries = entries_len / entry_size;
    assert!(entries > 0, "XSDT/RSDT contains no entries");

    for i in 0..entries {
        let entry_off = 36 + i * entry_size;
        let table_addr = if entry_size == 8 {
            read_u64_le(&sdt, entry_off)
        } else {
            u64::from(read_u32_le(&sdt, entry_off))
        };
        let sig = machine.read_physical_bytes(table_addr, 4);
        if &sig == b"APIC" {
            return table_addr;
        }
    }

    panic!("MADT (APIC) not found in XSDT/RSDT");
}

fn madt_lapic_base_and_cpu_apic_ids(machine: &mut Machine, madt_addr: u64) -> (u32, BTreeSet<u8>) {
    let hdr = machine.read_physical_bytes(madt_addr, 36);
    assert_eq!(&hdr[0..4], b"APIC");
    let length = read_u32_le(&hdr, 4) as usize;
    assert!(length >= 44, "MADT length too small: {length}");

    let madt = machine.read_physical_bytes(madt_addr, length);
    assert_eq!(checksum(&madt), 0, "MADT checksum");

    let lapic_base = read_u32_le(&madt, 36);

    // Entries start at offset 44 (header + lapic_base + flags).
    let mut off = 44usize;
    let mut apic_ids = BTreeSet::new();
    while off < madt.len() {
        assert!(off + 2 <= madt.len(), "truncated MADT entry header");
        let entry_type = madt[off];
        let entry_len = madt[off + 1] as usize;
        assert!(
            entry_len >= 2,
            "invalid MADT entry length {entry_len} at offset {off}"
        );
        assert!(
            off + entry_len <= madt.len(),
            "MADT entry overruns table: off={off} len={entry_len} table_len={}",
            madt.len()
        );

        if entry_type == 0 {
            assert!(
                entry_len >= 8,
                "Processor Local APIC entry too small: {entry_len}"
            );
            let acpi_processor_id = madt[off + 2];
            let apic_id = madt[off + 3];
            let flags = read_u32_le(&madt, off + 4);
            assert_eq!(
                acpi_processor_id, apic_id,
                "ACPI processor ID must match APIC ID (Aero contract)"
            );
            assert_ne!(
                flags & 1,
                0,
                "Processor Local APIC entry for APIC ID {apic_id} is not enabled"
            );
            apic_ids.insert(apic_id);
        }

        off += entry_len;
    }

    (lapic_base, apic_ids)
}

#[test]
fn bios_acpi_madt_exposes_configured_cpu_count() {
    let mut machine = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_acpi: true,
        cpu_count: 4,
        ..Default::default()
    })
    .expect("Machine::new should succeed");

    let rsdp_addr = find_rsdp(&mut machine);
    let madt_addr = find_madt(&mut machine, rsdp_addr);

    let (lapic_base, apic_ids) = madt_lapic_base_and_cpu_apic_ids(&mut machine, madt_addr);
    assert_eq!(lapic_base, LOCAL_APIC_BASE);

    let expected: BTreeSet<u8> = [0, 1, 2, 3].into_iter().collect();
    assert_eq!(apic_ids, expected);
}
