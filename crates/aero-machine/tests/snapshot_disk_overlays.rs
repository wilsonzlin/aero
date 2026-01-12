use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;
use std::io::{Cursor, Read, Seek, SeekFrom};

fn minimal_machine_config() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        // Keep the machine minimal/deterministic for unit tests.
        enable_pc_platform: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

#[test]
fn snapshot_includes_canonical_disk_overlay_refs() {
    let mut m = Machine::new(minimal_machine_config()).unwrap();
    m.set_ahci_port0_disk_overlay_ref("os.base", "os.overlay");
    m.set_ide_secondary_master_atapi_overlay_ref("iso.base", "iso.overlay");

    let snap = m.take_snapshot_full().unwrap();

    let disks = {
        let mut r = Cursor::new(snap.as_slice());
        let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");
        let disks = index
            .sections
            .iter()
            .find(|s| s.id == snapshot::SectionId::DISKS)
            .expect("snapshot should contain a DISKS section");
        r.seek(SeekFrom::Start(disks.offset))
            .expect("seek to DISKS payload");
        let mut limited = r.take(disks.len);
        snapshot::DiskOverlayRefs::decode(&mut limited).expect("decode DISKS payload")
    };

    assert_eq!(disks.disks.len(), 2);
    assert_eq!(disks.disks[0].disk_id, 0);
    assert_eq!(disks.disks[0].base_image, "os.base");
    assert_eq!(disks.disks[0].overlay_image, "os.overlay");
    assert_eq!(disks.disks[1].disk_id, 1);
    assert_eq!(disks.disks[1].base_image, "iso.base");
    assert_eq!(disks.disks[1].overlay_image, "iso.overlay");
}

#[test]
fn restore_exposes_disk_overlay_refs_for_host_reattach() {
    let mut src = Machine::new(minimal_machine_config()).unwrap();
    src.set_ahci_port0_disk_overlay_ref("os.base", "os.overlay");
    src.set_ide_secondary_master_atapi_overlay_ref("iso.base", "iso.overlay");
    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(minimal_machine_config()).unwrap();
    assert!(
        restored.restored_disk_overlays().is_none(),
        "fresh machine should not report restored disk overlays"
    );

    restored.restore_snapshot_bytes(&snap).unwrap();
    let overlays = restored
        .restored_disk_overlays()
        .expect("restored overlays should be available after restore");

    assert_eq!(overlays.disks.len(), 2);
    assert_eq!(overlays.disks[0].disk_id, 0);
    assert_eq!(overlays.disks[0].base_image, "os.base");
    assert_eq!(overlays.disks[0].overlay_image, "os.overlay");
    assert_eq!(overlays.disks[1].disk_id, 1);
    assert_eq!(overlays.disks[1].base_image, "iso.base");
    assert_eq!(overlays.disks[1].overlay_image, "iso.overlay");

    // Resetting the machine should clear restore-only overlay refs.
    restored.reset();
    assert!(
        restored.restored_disk_overlays().is_none(),
        "reset should clear restored disk overlay refs"
    );
}
