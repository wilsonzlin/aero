//! Smoke tests for SMP-facing firmware plumbing in `PcMachine`.
//!
//! `PcMachine` remains a single-vCPU executor today, but it should be possible to configure
//! `cpu_count > 1` so BIOS can publish SMP-capable ACPI/SMBIOS tables for experimentation.
#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{PcMachine, PcMachineConfig};
use firmware::bios::EBDA_BASE;

fn checksum_ok(data: &[u8]) -> bool {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0
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

fn read_phys(pc: &mut PcMachine, paddr: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    pc.bus.platform.memory.read_physical(paddr, &mut buf);
    buf
}

#[test]
fn pc_machine_cpu_count_2_publishes_madt_with_two_cpus() {
    let mut pc = PcMachine::new_with_config(PcMachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        cpu_count: 2,
        smbios_uuid_seed: 0,
        enable_hda: false,
        enable_e1000: false,
        enable_xhci: false,
    })
    .expect("PcMachine should allow cpu_count > 1 for firmware enumeration");

    // --- RSDP ---
    let rsdp = read_phys(&mut pc, EBDA_BASE + 0x100, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");
    assert!(checksum_ok(&rsdp[..20]));
    assert!(checksum_ok(&rsdp));
    let xsdt_addr = read_u64_le(&rsdp, 24);

    // --- XSDT ---
    let xsdt_hdr = read_phys(&mut pc, xsdt_addr, 36);
    assert_eq!(&xsdt_hdr[0..4], b"XSDT");
    let xsdt_len = read_u32_le(&xsdt_hdr, 4) as usize;
    assert!(xsdt_len >= 36);
    let xsdt = read_phys(&mut pc, xsdt_addr, xsdt_len);

    let entry_count = (xsdt_len - 36) / 8;
    assert!(entry_count > 0, "XSDT should have at least one SDT entry");

    // Locate the MADT ("APIC") table via the XSDT pointers.
    let mut madt_addr = None;
    for i in 0..entry_count {
        let addr = read_u64_le(&xsdt, 36 + i * 8);
        let sig = read_phys(&mut pc, addr, 4);
        if sig.as_slice() == b"APIC" {
            madt_addr = Some(addr);
            break;
        }
    }
    let madt_addr = madt_addr.expect("XSDT should reference the MADT/APIC table");

    // --- MADT ---
    let madt_hdr = read_phys(&mut pc, madt_addr, 36);
    assert_eq!(&madt_hdr[0..4], b"APIC");
    let madt_len = read_u32_le(&madt_hdr, 4) as usize;
    assert!(madt_len >= 44);
    let madt = read_phys(&mut pc, madt_addr, madt_len);

    // Count processor local APIC (type 0) entries.
    let mut off = 44usize;
    let mut cpu_ids = Vec::new();
    while off < madt.len() {
        assert!(
            off + 2 <= madt.len(),
            "MADT entry header out of bounds at off={off}"
        );
        let entry_type = madt[off];
        let entry_len = madt[off + 1] as usize;
        assert!(entry_len >= 2, "invalid MADT entry_len={entry_len}");
        assert!(
            off + entry_len <= madt.len(),
            "MADT entry truncated (off={off} len={entry_len} total={})",
            madt.len()
        );

        if entry_type == 0 {
            // Processor Local APIC structure:
            //   [2] ACPI Processor ID
            //   [3] APIC ID
            let acpi_id = madt[off + 2];
            cpu_ids.push(acpi_id);
        }

        off += entry_len;
    }

    cpu_ids.sort_unstable();
    assert_eq!(cpu_ids, vec![0, 1]);
}
