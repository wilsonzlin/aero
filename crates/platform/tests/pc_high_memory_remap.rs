use aero_platform::address_filter::AddressFilter;
use aero_platform::memory::MemoryBus;
use aero_platform::ChipsetState;
use aero_pc_constants::PCIE_ECAM_BASE;
use memory::SparseMemory;

#[test]
fn pc_high_memory_remaps_ram_above_4gib_and_hole_is_open_bus() {
    let chipset = ChipsetState::new(true);
    let filter = AddressFilter::new(chipset.a20());

    // Slightly over the PCIe ECAM base so we get a non-empty high-RAM segment without allocating
    // multi-gigabyte dense storage.
    let ram_bytes = PCIE_ECAM_BASE + 0x2000;
    let ram = SparseMemory::new(ram_bytes).unwrap();
    let mut bus = MemoryBus::with_ram(filter, Box::new(ram));

    // High RAM starts at 4GiB and aliases to the backing RAM bytes above PCIE_ECAM_BASE.
    let high_base = 0x1_0000_0000u64;
    let pattern: Vec<u8> = (0..16).map(|v| 0xA0u8.wrapping_add(v as u8)).collect();
    bus.write_physical(high_base, &pattern);
    let mut readback = vec![0u8; pattern.len()];
    bus.read_physical(high_base, &mut readback);
    assert_eq!(readback, pattern);

    // The PCI hole below 4GiB behaves as open bus unless devices are mapped there.
    let mut hole = [0u8; 4];
    bus.read_physical(PCIE_ECAM_BASE + 0x1000, &mut hole);
    assert_eq!(hole, [0xFF; 4]);

    // Reads that straddle low RAM -> hole should return mapped bytes then open-bus 0xFFs.
    bus.write_physical(PCIE_ECAM_BASE - 4, &[1, 2, 3, 4]);
    let mut straddle_low_hole = [0u8; 8];
    bus.read_physical(PCIE_ECAM_BASE - 4, &mut straddle_low_hole);
    assert_eq!(&straddle_low_hole[..4], &[1, 2, 3, 4]);
    assert_eq!(&straddle_low_hole[4..], &[0xFF; 4]);

    // Reads that straddle hole -> high RAM should include open-bus bytes followed by high RAM.
    let mut straddle_hole_high = [0u8; 32];
    bus.read_physical(high_base - 16, &mut straddle_hole_high);
    assert_eq!(&straddle_hole_high[..16], &[0xFF; 16]);
    assert_eq!(&straddle_hole_high[16..], &pattern);

    // Straddling writes should update only the mapped high-RAM portion.
    let write_pattern: Vec<u8> = (0..32).map(|v| v as u8).collect();
    bus.write_physical(high_base - 16, &write_pattern);
    let mut high_readback = vec![0u8; 16];
    bus.read_physical(high_base, &mut high_readback);
    assert_eq!(high_readback, write_pattern[16..]);

    // A20 masking still applies to all physical accesses.
    bus.a20().set_enabled(false);
    bus.write_u8(0x0, 0x11);
    assert_eq!(bus.read_u8(0x1_00000), 0x11);
}
