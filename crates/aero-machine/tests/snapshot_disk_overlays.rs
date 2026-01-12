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
    m.set_ide_primary_master_ata_overlay_ref("ide.base", "ide.overlay");

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

    assert_eq!(disks.disks.len(), 3);
    assert_eq!(disks.disks[0].disk_id, 0);
    assert_eq!(disks.disks[0].base_image, "os.base");
    assert_eq!(disks.disks[0].overlay_image, "os.overlay");
    assert_eq!(disks.disks[1].disk_id, 1);
    assert_eq!(disks.disks[1].base_image, "iso.base");
    assert_eq!(disks.disks[1].overlay_image, "iso.overlay");
    assert_eq!(disks.disks[2].disk_id, 2);
    assert_eq!(disks.disks[2].base_image, "ide.base");
    assert_eq!(disks.disks[2].overlay_image, "ide.overlay");
}

#[test]
fn restore_exposes_disk_overlay_refs_for_host_reattach() {
    let mut src = Machine::new(minimal_machine_config()).unwrap();
    src.set_ahci_port0_disk_overlay_ref("os.base", "os.overlay");
    src.set_ide_secondary_master_atapi_overlay_ref("iso.base", "iso.overlay");
    src.set_ide_primary_master_ata_overlay_ref("ide.base", "ide.overlay");
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

    assert_eq!(overlays.disks.len(), 3);
    assert_eq!(overlays.disks[0].disk_id, 0);
    assert_eq!(overlays.disks[0].base_image, "os.base");
    assert_eq!(overlays.disks[0].overlay_image, "os.overlay");
    assert_eq!(overlays.disks[1].disk_id, 1);
    assert_eq!(overlays.disks[1].base_image, "iso.base");
    assert_eq!(overlays.disks[1].overlay_image, "iso.overlay");
    assert_eq!(overlays.disks[2].disk_id, 2);
    assert_eq!(overlays.disks[2].base_image, "ide.base");
    assert_eq!(overlays.disks[2].overlay_image, "ide.overlay");

    // Resetting the machine should clear restore-only overlay refs.
    restored.reset();
    assert!(
        restored.restored_disk_overlays().is_none(),
        "reset should clear restored disk overlay refs"
    );
}

#[test]
fn restore_sorts_disk_overlay_refs_by_disk_id_even_if_snapshot_file_is_unsorted() {
    let mut src = Machine::new(minimal_machine_config()).unwrap();
    src.set_ahci_port0_disk_overlay_ref("os.base", "os.overlay");
    src.set_ide_secondary_master_atapi_overlay_ref("iso.base", "iso.overlay");
    src.set_ide_primary_master_ata_overlay_ref("ide.base", "ide.overlay");
    let mut snap = src.take_snapshot_full().unwrap();

    // Locate the DISKS section and rewrite its payload with the same entries but in reverse order.
    let (disks_off, disks_len) = {
        let mut r = Cursor::new(snap.as_slice());
        let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");
        let disks = index
            .sections
            .iter()
            .find(|s| s.id == snapshot::SectionId::DISKS)
            .expect("snapshot should contain a DISKS section");
        (disks.offset, disks.len)
    };

    let mut overlays = {
        let mut r = Cursor::new(snap.as_slice());
        r.seek(SeekFrom::Start(disks_off))
            .expect("seek to DISKS payload");
        let mut limited = r.take(disks_len);
        snapshot::DiskOverlayRefs::decode(&mut limited).expect("decode DISKS payload")
    };
    overlays.disks.reverse();

    let mut rewritten = Vec::new();
    overlays.encode(&mut rewritten).expect("encode DISKS payload");
    assert_eq!(
        u64::try_from(rewritten.len()).unwrap(),
        disks_len,
        "rewriting DISKS section should not change the payload length"
    );
    let off = usize::try_from(disks_off).expect("DISKS offset should fit in usize");
    let len = usize::try_from(disks_len).expect("DISKS len should fit in usize");
    snap[off..off + len].copy_from_slice(&rewritten);

    // Restore and verify the machine canonicalizes the order (ascending disk_id) for host usage.
    let mut restored = Machine::new(minimal_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
    let overlays = restored
        .take_restored_disk_overlays()
        .expect("restored overlays should be available");

    let disk_ids: Vec<u32> = overlays.disks.iter().map(|d| d.disk_id).collect();
    assert_eq!(disk_ids, vec![0, 1, 2]);
}
