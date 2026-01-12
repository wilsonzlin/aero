use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

/// Regression test: snapshot RAM serialization must bypass A20-masked physical accesses.
///
/// If snapshot save uses `MemoryBus::read_physical` while A20 is disabled, reads at `0x100000`
/// alias to `0x00000` and corrupt the captured >1MiB RAM contents.
#[test]
fn snapshot_ram_bypasses_a20_masking_when_a20_disabled_at_snapshot_time() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Enable A20 via the "fast A20 gate" at port 0x92.
    src.io_write(0x92, 1, 0x02);
    src.write_physical_u8(0x00000, 0x11);
    src.write_physical_u8(0x1_00000, 0x22);

    // Sanity-check that A20 is enabled: the two addresses are distinct.
    assert_eq!(src.read_physical_u8(0x00000), 0x11);
    assert_eq!(src.read_physical_u8(0x1_00000), 0x22);

    // Disable A20 and verify reads alias.
    src.io_write(0x92, 1, 0x00);
    assert_eq!(src.read_physical_u8(0x1_00000), 0x11);

    // Take a full snapshot while A20 is disabled. Snapshot RAM reads must still capture the true
    // >1MiB contents.
    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Re-enable A20 and ensure restored memory still contains distinct bytes.
    restored.io_write(0x92, 1, 0x02);
    assert_eq!(restored.read_physical_u8(0x00000), 0x11);
    assert_eq!(restored.read_physical_u8(0x1_00000), 0x22);
}
