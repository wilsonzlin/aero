use std::collections::BTreeSet;

use aero_machine::{Machine, MachineConfig};

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0
}

fn scan_region_for_smbios(machine: &mut Machine, base: u64, len: usize) -> Option<u64> {
    let buf = machine.read_physical_bytes(base, len);
    for off in (0..len.saturating_sub(4)).step_by(16) {
        if &buf[off..off + 4] == b"_SM_" {
            return Some(base + off as u64);
        }
    }
    None
}

fn find_smbios_eps(machine: &mut Machine) -> u64 {
    // SMBIOS spec: search the first KiB of EBDA first, then scan 0xF0000-0xFFFFF on 16-byte
    // boundaries.
    let ebda_seg = machine.read_physical_u16(0x040E);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u64) << 4;
        if let Some(addr) = scan_region_for_smbios(machine, ebda_base, 1024) {
            return addr;
        }
    }

    scan_region_for_smbios(machine, 0xF0000, 0x10000)
        .expect("SMBIOS EPS not found in EBDA or BIOS scan region")
}

#[derive(Debug)]
struct SmbiosStructure {
    ty: u8,
    handle: u16,
}

fn parse_smbios_table(table: &[u8]) -> Vec<SmbiosStructure> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < table.len() {
        assert!(
            i + 4 <= table.len(),
            "truncated SMBIOS structure header at offset {i}"
        );
        let ty = table[i];
        let len = table[i + 1] as usize;
        assert!(
            len >= 4,
            "invalid SMBIOS structure length {len} at offset {i} (type={ty})"
        );
        assert!(
            i + len <= table.len(),
            "SMBIOS structure overruns table: type={ty} offset={i} len={len} table_len={}",
            table.len()
        );
        let handle = u16::from_le_bytes([table[i + 2], table[i + 3]]);

        // Walk the string-set until the double-NUL terminator.
        let mut j = i + len;
        loop {
            assert!(
                j + 1 < table.len(),
                "unterminated SMBIOS string-set (type={ty} handle=0x{handle:04x})"
            );
            if table[j] == 0 && table[j + 1] == 0 {
                j += 2;
                break;
            }
            j += 1;
        }

        out.push(SmbiosStructure { ty, handle });

        i = j;
        if ty == 127 {
            break;
        }
    }
    out
}

#[test]
fn machine_smbios_exposes_configured_cpu_count() {
    let cpu_count = 4u8;

    let mut machine = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_acpi: true,
        cpu_count,
        ..Default::default()
    })
    .expect("Machine::new should succeed");

    // Discover the SMBIOS EPS using the spec-defined scan rules rather than relying on the
    // convenience accessor.
    let eps_addr = find_smbios_eps(&mut machine);

    // Parse the SMBIOS 2.x Entry Point Structure to locate the structure table.
    let eps = machine.read_physical_bytes(eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(checksum_ok(&eps), "SMBIOS EPS checksum failed");
    assert_eq!(&eps[0x10..0x15], b"_DMI_");
    assert!(
        checksum_ok(&eps[0x10..]),
        "SMBIOS intermediate checksum failed"
    );

    let table_len = u16::from_le_bytes([eps[0x16], eps[0x17]]) as usize;
    let table_addr = u32::from_le_bytes([eps[0x18], eps[0x19], eps[0x1A], eps[0x1B]]) as u64;
    let table = machine.read_physical_bytes(table_addr, table_len);

    let structures = parse_smbios_table(&table);
    let cpu_structs: Vec<_> = structures.iter().filter(|s| s.ty == 4).collect();
    assert_eq!(
        cpu_structs.len(),
        cpu_count as usize,
        "SMBIOS must expose one Type 4 (Processor Information) structure per configured CPU"
    );

    let handles: BTreeSet<u16> = cpu_structs.iter().map(|s| s.handle).collect();
    assert_eq!(
        handles.len(),
        cpu_count as usize,
        "SMBIOS Type 4 handles must be unique"
    );
}

