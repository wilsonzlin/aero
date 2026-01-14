use aero_machine::{Machine, MachineConfig};
use firmware::acpi::{
    parse_header, parse_rsdp_v2, parse_rsdt_entries, parse_xsdt_entries, ACPI_HEADER_SIZE,
    RSDP_V2_SIZE,
};
use pretty_assertions::assert_eq;

const RAM_SIZE_BYTES: u64 = 16 * 1024 * 1024;
const CPU_COUNT: u8 = 4;

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(raw.try_into().ok()?))
}

fn read_acpi_table(m: &mut Machine, paddr: u64) -> Option<Vec<u8>> {
    // Read and parse the SDT header to discover the full table length.
    let header = m.read_physical_bytes(paddr, ACPI_HEADER_SIZE);
    let hdr = parse_header(&header)?;

    let len: usize = hdr.length.try_into().ok()?;
    // Defensive bounds checks: reject absurd lengths and OOB reads.
    if len < ACPI_HEADER_SIZE || len > 64 * 1024 {
        return None;
    }
    let end = paddr.checked_add(hdr.length as u64)?;
    if end > RAM_SIZE_BYTES {
        return None;
    }

    Some(m.read_physical_bytes(paddr, len))
}

fn madt_local_apic_addr_and_processor_apic_ids(madt: &[u8]) -> Option<(u32, Vec<u8>)> {
    let hdr = parse_header(madt)?;
    if &hdr.signature != b"APIC" {
        return None;
    }
    if madt.len() < 44 {
        return None;
    }

    let local_apic_addr = read_u32_le(madt, 36)?;

    let mut apic_ids = Vec::new();
    let mut off = 44;
    while off < madt.len() {
        let entry_type = *madt.get(off)?;
        let entry_len = *madt.get(off + 1)? as usize;
        if entry_len < 2 {
            return None;
        }
        let entry_end = off.checked_add(entry_len)?;
        if entry_end > madt.len() {
            return None;
        }

        if entry_type == 0 {
            // Processor Local APIC entry (type 0).
            // Layout: type (1), len (1), acpi_id (1), apic_id (1), flags (4).
            if entry_len < 8 {
                return None;
            }
            let apic_id = *madt.get(off + 3)?;
            apic_ids.push(apic_id);
        }

        off = entry_end;
    }

    Some((local_apic_addr, apic_ids))
}

#[test]
fn acpi_madt_enumerates_machine_cpu_count() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE_BYTES,
        cpu_count: CPU_COUNT,
        enable_pc_platform: true,
        enable_acpi: true,
        ..Default::default()
    })
    .unwrap();

    // Use the firmware-published RSDP address so the test stays robust even if the BIOS moves
    // where it places the pointer (EBDA vs other regions).
    let rsdp_addr = m.acpi_rsdp_addr().expect("firmware should publish an RSDP");
    let rsdp_bytes = m.read_physical_bytes(rsdp_addr, RSDP_V2_SIZE);
    let rsdp = parse_rsdp_v2(&rsdp_bytes).expect("RSDP v2 should parse");

    // Prefer XSDT (ACPI 2.0+) and fall back to RSDT.
    let sdt_addrs: Vec<u64> = if rsdp.xsdt_address != 0 {
        let xsdt = read_acpi_table(&mut m, rsdp.xsdt_address).expect("XSDT table should read");
        parse_xsdt_entries(&xsdt).expect("XSDT entries should parse")
    } else {
        let rsdt =
            read_acpi_table(&mut m, rsdp.rsdt_address as u64).expect("RSDT table should read");
        parse_rsdt_entries(&rsdt)
            .expect("RSDT entries should parse")
            .into_iter()
            .map(u64::from)
            .collect()
    };

    // Find MADT (signature "APIC") in the SDT list.
    let madt_addr = sdt_addrs
        .iter()
        .copied()
        .find(|&addr| {
            let header = m.read_physical_bytes(addr, ACPI_HEADER_SIZE);
            parse_header(&header).is_some_and(|hdr| &hdr.signature == b"APIC")
        })
        .expect("XSDT/RSDT should reference a MADT (APIC) table");

    let madt = read_acpi_table(&mut m, madt_addr).expect("MADT should read");
    let (local_apic_addr, mut apic_ids) =
        madt_local_apic_addr_and_processor_apic_ids(&madt).expect("MADT should parse");

    assert_eq!(local_apic_addr, 0xFEE0_0000);

    apic_ids.sort_unstable();
    assert_eq!(apic_ids.len(), CPU_COUNT as usize);
    assert_eq!(apic_ids, (0..CPU_COUNT).collect::<Vec<u8>>());
}
