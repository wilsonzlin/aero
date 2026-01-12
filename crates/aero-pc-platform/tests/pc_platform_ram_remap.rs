use aero_pc_platform::{PcPlatform, PcPlatformConfig, PCIE_ECAM_BASE};
use memory::SparseMemory;

#[test]
fn pc_platform_remaps_ram_above_4gib_when_total_ram_exceeds_ecam_base() {
    const FOUR_GIB: u64 = 0x1_0000_0000;

    // Make RAM slightly larger than the ECAM base so the BIOS E820 builder would remap the excess
    // above 4GiB, but use a sparse backing store so we don't allocate multi-GB in the test.
    let total_ram = PCIE_ECAM_BASE + 0x2000;
    let ram = SparseMemory::with_chunk_size(total_ram, 2 * 1024 * 1024).unwrap();

    let mut platform = PcPlatform::new_with_config_and_ram(
        Box::new(ram),
        PcPlatformConfig {
            enable_hda: false,
            enable_nvme: false,
            enable_ahci: false,
            enable_ide: false,
            enable_e1000: false,
            mac_addr: None,
            enable_uhci: false,
            enable_virtio_blk: false,
        },
    );

    // A20 gating doesn't affect these addresses (bit 20 is clear), but keep it enabled to avoid
    // any surprising aliasing if this test is extended.
    platform.chipset.a20().set_enabled(true);

    // When the remap is active, guest-physical RAM should be exposed in two ranges:
    // - [0, PCIE_ECAM_BASE)
    // - [4GiB, 4GiB + (total_ram - PCIE_ECAM_BASE))
    let phys_size = FOUR_GIB + (total_ram - PCIE_ECAM_BASE);
    assert_eq!(platform.memory.ram().size(), phys_size);

    // Verify we can write to and read from RAM above 4GiB.
    let addr = FOUR_GIB;
    let pattern = [0xAAu8, 0xBB, 0xCC, 0xDD];
    platform.memory.write_physical(addr, &pattern);
    let mut buf = [0u8; 4];
    platform.memory.read_physical(addr, &mut buf);
    assert_eq!(buf, pattern);

    // Verify the end of the remapped region is also accessible.
    let last = phys_size - 1;
    platform.memory.write_physical(last, &[0x42]);
    let mut b = [0u8; 1];
    platform.memory.read_physical(last, &mut b);
    assert_eq!(b, [0x42]);
}

