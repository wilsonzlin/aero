use aero_machine::{Machine, MachineConfig};
use aero_pc_constants::{PCIE_ECAM_BASE, PCIE_ECAM_END_BUS, PCIE_ECAM_SEGMENT, PCIE_ECAM_START_BUS};

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
fn machine_acpi_mcfg_publishes_canonical_ecam_window() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_acpi: true,
        ..Default::default()
    })
    .unwrap();

    let rsdp_addr = m.acpi_rsdp_addr().expect("expected ACPI RSDP to be present");
    let rsdp = m.read_physical_bytes(rsdp_addr, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");
    assert!(checksum_ok(&rsdp[..20]));
    assert!(checksum_ok(&rsdp));

    let xsdt_addr = u64::from_le_bytes(rsdp[24..32].try_into().unwrap());
    assert_ne!(xsdt_addr, 0);

    let xsdt = read_sdt(&mut m, xsdt_addr);
    assert_eq!(&xsdt[0..4], b"XSDT");

    // Parse XSDT entries (u64 pointers) and locate the MCFG (PCI Express MMCONFIG/ECAM) table.
    let entries = &xsdt[36..];
    assert!(
        entries.len() % 8 == 0,
        "XSDT entry region must be 8-byte aligned"
    );

    let mut mcfg_addr: Option<u64> = None;
    for ent in entries.chunks_exact(8) {
        let addr = u64::from_le_bytes(ent.try_into().unwrap());
        if addr == 0 {
            continue;
        }
        let sig = m.read_physical_bytes(addr, 4);
        if sig.as_slice() == b"MCFG" {
            mcfg_addr = Some(addr);
            break;
        }
    }

    let mcfg_addr = mcfg_addr.expect("missing MCFG table in XSDT");
    let mcfg = read_sdt(&mut m, mcfg_addr);
    assert_eq!(&mcfg[0..4], b"MCFG");

    // MCFG payload is:
    // - SDT header: 36 bytes
    // - reserved: 8 bytes
    // - one or more allocation entries: 16 bytes each
    const MCFG_MIN_LEN: usize = 36 + 8 + 16;
    assert!(
        mcfg.len() >= MCFG_MIN_LEN,
        "MCFG length too small: {} (need at least {MCFG_MIN_LEN})",
        mcfg.len()
    );

    // First configuration space allocation structure.
    let alloc0 = 36 + 8;
    let base = u64::from_le_bytes(mcfg[alloc0..alloc0 + 8].try_into().unwrap());
    let segment = u16::from_le_bytes(mcfg[alloc0 + 8..alloc0 + 10].try_into().unwrap());
    let start_bus = mcfg[alloc0 + 10];
    let end_bus = mcfg[alloc0 + 11];

    assert_eq!(base, PCIE_ECAM_BASE);
    assert_eq!(segment, PCIE_ECAM_SEGMENT);
    assert_eq!(start_bus, PCIE_ECAM_START_BUS);
    assert_eq!(end_bus, PCIE_ECAM_END_BUS);
}

