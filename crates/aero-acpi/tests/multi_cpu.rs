use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

#[test]
fn multi_cpu_tables_emit_topology_in_madt_and_dsdt() {
    let cpu_count = 4u8;
    let cfg = AcpiConfig {
        cpu_count,
        ..Default::default()
    };
    let tables = AcpiTables::build(&cfg, AcpiPlacement::default());

    // --- MADT ---
    let madt = &tables.madt;
    assert!(madt.len() >= 44, "MADT too short");
    assert_eq!(&madt[0..4], b"APIC");

    let mut found = vec![false; cpu_count as usize];
    let mut lapic_count = 0usize;

    // Subtables start after:
    // - SDT header (36 bytes)
    // - local APIC address (u32)
    // - flags (u32)
    let mut off = 44usize;
    while off < madt.len() {
        let entry_type = madt[off];
        let entry_len = madt[off + 1] as usize;
        assert!(
            entry_len >= 2,
            "MADT entry at offset {off} has invalid length {entry_len}"
        );
        assert!(
            off + entry_len <= madt.len(),
            "MADT entry at offset {off} overruns table (len={entry_len})"
        );

        if entry_type == 0 {
            // Processor Local APIC structure:
            // [0] type=0, [1] len=8, [2] ACPI Processor ID, [3] APIC ID, [4..8] flags
            assert_eq!(
                entry_len, 8,
                "Processor Local APIC entry should be 8 bytes"
            );
            let acpi_id = madt[off + 2];
            let apic_id = madt[off + 3];
            let flags = read_u32_le(madt, off + 4);

            assert_eq!(
                acpi_id, apic_id,
                "MADT LAPIC entry must have ACPI Processor ID == APIC ID"
            );
            assert!(
                acpi_id < cpu_count,
                "MADT LAPIC entry has out-of-range CPU id {acpi_id} (cpu_count={cpu_count})"
            );
            assert!(
                (flags & 0x1) != 0,
                "MADT LAPIC entry for CPU {acpi_id} must have flags bit0 (enabled) set"
            );
            assert!(
                !found[acpi_id as usize],
                "duplicate MADT LAPIC entry for CPU {acpi_id}"
            );
            found[acpi_id as usize] = true;
            lapic_count += 1;
        }

        off += entry_len;
    }

    assert_eq!(
        lapic_count,
        cpu_count as usize,
        "MADT should contain exactly cpu_count Processor Local APIC entries"
    );
    assert!(
        found.iter().all(|&present| present),
        "MADT missing at least one Processor Local APIC entry for CPUs 0..cpu_count"
    );

    // --- DSDT AML ---
    assert!(tables.dsdt.len() >= 36, "DSDT too short");
    let aml = &tables.dsdt[36..];

    let processor_op = [0x5B, 0x83];
    let processor_count = aml
        .windows(processor_op.len())
        .filter(|w| *w == processor_op)
        .count();
    assert_eq!(
        processor_count,
        cpu_count as usize,
        "DSDT AML should contain exactly cpu_count ProcessorOp encodings"
    );

    // Verify CPU name segments for CPU IDs < 16 ("CPU0".."CPUF").
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for cpu_id in 0..cpu_count {
        let name = [b'C', b'P', b'U', HEX[cpu_id as usize]];
        assert!(
            aml.windows(4).any(|w| w == name),
            "missing CPU object name segment {:?} in DSDT AML",
            core::str::from_utf8(&name).unwrap()
        );
    }
}

