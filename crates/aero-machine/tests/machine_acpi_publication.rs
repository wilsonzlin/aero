use aero_machine::{Machine, MachineConfig};

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)) == 0
}

/// Read an ACPI SDT (System Description Table) from guest RAM.
///
/// This implements the minimal parsing required for integration tests:
/// - read the standard 36-byte SDT header,
/// - use the `Length` field to read the full table,
/// - and verify the checksum.
fn read_sdt(m: &mut Machine, addr: u64) -> Vec<u8> {
    // SDT header is always 36 bytes.
    let hdr = m.read_physical_bytes(addr, 36);
    assert_eq!(hdr.len(), 36);
    let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
    assert!(len >= 36, "ACPI SDT length too small: {len}");
    let table = m.read_physical_bytes(addr, len);
    assert!(
        checksum_ok(&table),
        "ACPI SDT checksum invalid for {:?} at 0x{addr:x}",
        &table[0..4]
    );
    table
}

#[test]
fn machine_config_explicitly_controls_acpi_publication() {
    // Even when the PC platform is wired, ACPI publication should be explicitly controlled.
    let m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_acpi: false,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(m.acpi_rsdp_addr(), None);

    // When enabled, the BIOS should publish the RSDP and the key tables should be readable.
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_acpi: true,
        ..Default::default()
    })
    .unwrap();

    let rsdp_addr = m
        .acpi_rsdp_addr()
        .expect("expected ACPI RSDP to be present");
    let rsdp = m.read_physical_bytes(rsdp_addr, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");
    assert!(checksum_ok(&rsdp[..20]));
    assert!(checksum_ok(&rsdp));

    let rsdt_addr = u64::from(u32::from_le_bytes(rsdp[16..20].try_into().unwrap()));
    let xsdt_addr = u64::from_le_bytes(rsdp[24..32].try_into().unwrap());
    assert_ne!(rsdt_addr, 0);
    assert_ne!(xsdt_addr, 0);

    let rsdt = read_sdt(&mut m, rsdt_addr);
    assert_eq!(&rsdt[0..4], b"RSDT");

    let xsdt = read_sdt(&mut m, xsdt_addr);
    assert_eq!(&xsdt[0..4], b"XSDT");

    // Parse XSDT entries (u64 pointers) and ensure key tables exist.
    let entries = &xsdt[36..];
    assert!(
        entries.len() % 8 == 0,
        "XSDT entry region must be 8-byte aligned"
    );
    let mut fadt_addr: Option<u64> = None;
    let mut madt_addr: Option<u64> = None;
    let mut hpet_addr: Option<u64> = None;
    for ent in entries.chunks_exact(8) {
        let addr = u64::from_le_bytes(ent.try_into().unwrap());
        if addr == 0 {
            continue;
        }
        let sig = m.read_physical_bytes(addr, 4);
        match sig.as_slice() {
            b"FACP" => fadt_addr = Some(addr),
            b"APIC" => madt_addr = Some(addr),
            b"HPET" => hpet_addr = Some(addr),
            _ => {}
        }
        // Prove the referenced table is readable by validating its checksum.
        let _ = read_sdt(&mut m, addr);
    }

    let fadt_addr = fadt_addr.expect("missing FADT (FACP) in XSDT");
    madt_addr.expect("missing MADT (APIC) in XSDT");
    hpet_addr.expect("missing HPET table in XSDT");

    // DSDT is referenced from the FADT (not directly from XSDT/RSDT).
    let fadt = read_sdt(&mut m, fadt_addr);
    assert_eq!(&fadt[0..4], b"FACP");

    // FADT revision 3 (ACPI 2.0) includes both 32-bit and 64-bit DSDT pointers.
    let dsdt_32 = u64::from(u32::from_le_bytes(fadt[40..44].try_into().unwrap()));
    let dsdt_64 = if fadt.len() >= 148 {
        u64::from_le_bytes(fadt[140..148].try_into().unwrap())
    } else {
        0
    };
    let dsdt_addr = if dsdt_64 != 0 { dsdt_64 } else { dsdt_32 };
    assert_ne!(dsdt_addr, 0);

    let dsdt = read_sdt(&mut m, dsdt_addr);
    assert_eq!(&dsdt[0..4], b"DSDT");
}
