use aero_pc_platform::{PcPlatform, PcPlatformConfig, PcPlatformSnapshotHarness, PCIE_ECAM_BASE};
use aero_snapshot::{SnapshotSource, SnapshotTarget};
use memory::SparseMemory;

#[test]
fn pc_platform_snapshot_harness_translates_dense_ram_offsets_when_remap_is_active() {
    const FOUR_GIB: u64 = 0x1_0000_0000;

    // Make RAM slightly larger than the ECAM base so the platform's RAM backend uses the
    // [0, PCIE_ECAM_BASE) + [4GiB, ...) layout. Use SparseMemory so we don't allocate multi-GB.
    let total_ram = PCIE_ECAM_BASE + 0x4000;
    let ram = SparseMemory::with_chunk_size(total_ram, 2 * 1024 * 1024).unwrap();

    let mut pc = PcPlatform::new_with_config_and_ram(
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
    pc.chipset.a20().set_enabled(true);

    // Write distinct bytes into:
    // - the end of low RAM
    // - the start of the remapped region at >= 4GiB
    pc.memory.write_physical(PCIE_ECAM_BASE - 2, &[0x11, 0x22]);
    pc.memory.write_physical(FOUR_GIB, &[0x33, 0x44]);

    let mut harness = PcPlatformSnapshotHarness::new(&mut pc);

    // Snapshot RAM is represented as a dense byte array of `total_ram` bytes (not including the
    // below-4GiB hole).
    assert_eq!(SnapshotSource::ram_len(&harness), total_ram as usize);

    // Read across the dense offset where the remap happens (PCIE_ECAM_BASE).
    let mut buf = [0u8; 4];
    SnapshotSource::read_ram(&harness, PCIE_ECAM_BASE - 2, &mut buf).unwrap();
    assert_eq!(buf, [0x11, 0x22, 0x33, 0x44]);

    // Writes across the boundary should update low RAM and the remapped high region (and not touch
    // the hole).
    SnapshotTarget::write_ram(&mut harness, PCIE_ECAM_BASE - 2, &[0xAA, 0xBB, 0xCC, 0xDD]).unwrap();

    let mut low = [0u8; 2];
    harness
        .platform()
        .memory
        .read_physical(PCIE_ECAM_BASE - 2, &mut low);
    let mut high = [0u8; 2];
    harness.platform().memory.read_physical(FOUR_GIB, &mut high);

    assert_eq!(low, [0xAA, 0xBB]);
    assert_eq!(high, [0xCC, 0xDD]);
}
