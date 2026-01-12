use aero_devices::a20_gate::A20_GATE_PORT;
use aero_machine::{Machine, MachineConfig};
use firmware::bios::{PCIE_ECAM_BASE, RESET_VECTOR_ALIAS_PHYS, RESET_VECTOR_PHYS};
use pretty_assertions::assert_eq;

#[test]
fn machine_high_memory_remap_does_not_require_dense_multi_gib_allocations() {
    // Use a RAM size that crosses the canonical PCIe ECAM base so the machine must:
    // - leave the ECAM/PCI hole unmapped (open bus),
    // - remap the remainder above 4GiB, and
    // - still map the BIOS ROM reset vector alias at the top of the 32-bit space.
    //
    // This configuration is deliberately huge to catch regressions where `Machine::new` tries to
    // allocate and zero multi-gigabyte guest RAM eagerly.
    let ram_size_bytes = PCIE_ECAM_BASE + 0x2000;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes,
        ..Default::default()
    })
    .expect("machine init should succeed with large (sparse) RAM");

    // Enable A20 via the fast A20 port (0x92) so our physical addresses are not masked.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // The PCIe ECAM window begins at PCIE_ECAM_BASE and should behave as open bus when no MMIO
    // device claims it.
    assert_eq!(
        m.read_physical_bytes(PCIE_ECAM_BASE + 0x1000, 4),
        vec![0xFF; 4]
    );
    m.write_physical(PCIE_ECAM_BASE + 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    assert_eq!(
        m.read_physical_bytes(PCIE_ECAM_BASE + 0x1000, 4),
        vec![0xFF; 4]
    );

    // Low RAM stops at PCIE_ECAM_BASE; reads that straddle into the hole should include open-bus
    // bytes.
    m.write_physical(PCIE_ECAM_BASE - 4, &[1, 2, 3, 4]);
    let straddle_low_hole = m.read_physical_bytes(PCIE_ECAM_BASE - 4, 8);
    assert_eq!(&straddle_low_hole[..4], &[1, 2, 3, 4]);
    assert_eq!(&straddle_low_hole[4..], &[0xFF; 4]);

    // High RAM begins at 4GiB.
    const HIGH_BASE: u64 = 0x1_0000_0000;
    let pattern: Vec<u8> = (0..16).map(|v| 0xA0u8.wrapping_add(v as u8)).collect();
    m.write_physical(HIGH_BASE, &pattern);
    assert_eq!(m.read_physical_bytes(HIGH_BASE, pattern.len()), pattern);

    // The BIOS ROM is mapped at both the conventional reset vector (F000:FFF0 alias) and at the
    // top-of-4GiB reset-vector alias. Verify those bytes still match when high RAM is present.
    let reset_low = m.read_physical_bytes(RESET_VECTOR_PHYS, 16);
    let reset_high = m.read_physical_bytes(RESET_VECTOR_ALIAS_PHYS, 16);
    assert_eq!(reset_high, reset_low);

    // Reading across the 4GiB boundary should see the ROM alias first, then high RAM.
    let straddle_rom_high = m.read_physical_bytes(RESET_VECTOR_ALIAS_PHYS, 32);
    assert_eq!(&straddle_rom_high[..16], &reset_low);
    assert_eq!(&straddle_rom_high[16..], &pattern);
}
