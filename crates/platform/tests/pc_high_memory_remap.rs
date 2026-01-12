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
    bus.write_physical(high_base, &[0xAA, 0xBB, 0xCC]);
    let mut readback = [0u8; 3];
    bus.read_physical(high_base, &mut readback);
    assert_eq!(readback, [0xAA, 0xBB, 0xCC]);

    // The PCI hole below 4GiB behaves as open bus unless devices are mapped there.
    let mut hole = [0u8; 4];
    bus.read_physical(PCIE_ECAM_BASE + 0x1000, &mut hole);
    assert_eq!(hole, [0xFF; 4]);

    // A20 masking still applies to all physical accesses.
    bus.a20().set_enabled(false);
    bus.write_u8(0x0, 0x11);
    assert_eq!(bus.read_u8(0x1_00000), 0x11);
}

