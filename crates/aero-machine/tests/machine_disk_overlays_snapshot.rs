#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
use pretty_assertions::assert_eq;

struct CaptureDiskOverlaysTarget {
    ram: Vec<u8>,
    disks: Option<snapshot::DiskOverlayRefs>,
}

impl CaptureDiskOverlaysTarget {
    fn new(ram_len: usize) -> Self {
        Self {
            ram: vec![0; ram_len],
            disks: None,
        }
    }
}

impl snapshot::SnapshotTarget for CaptureDiskOverlaysTarget {
    fn restore_cpu_state(&mut self, _state: snapshot::CpuState) {}

    fn restore_mmu_state(&mut self, _state: snapshot::MmuState) {}

    fn restore_device_states(&mut self, _states: Vec<snapshot::DeviceState>) {}

    fn restore_disk_overlays(&mut self, overlays: snapshot::DiskOverlayRefs) {
        self.disks = Some(overlays);
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> snapshot::Result<()> {
        let offset = usize::try_from(offset)
            .map_err(|_| snapshot::SnapshotError::Corrupt("ram write offset overflow"))?;
        let end = offset
            .checked_add(data.len())
            .ok_or(snapshot::SnapshotError::Corrupt(
                "ram write offset overflow",
            ))?;
        if end > self.ram.len() {
            return Err(snapshot::SnapshotError::Corrupt("ram write out of range"));
        }
        self.ram[offset..end].copy_from_slice(data);
        Ok(())
    }
}

#[test]
fn machine_snapshot_writes_and_restores_disk_overlay_refs_with_stable_disk_ids() {
    let mut cfg = MachineConfig::win7_storage_defaults(2 * 1024 * 1024);
    // Keep the test focused on snapshot disk overlay plumbing.
    cfg.enable_serial = false;
    cfg.enable_i8042 = false;
    cfg.enable_a20_gate = false;
    cfg.enable_reset_ctrl = false;
    cfg.enable_vga = false;

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Attach a dummy disk backend to AHCI port 0 so this test mirrors a real configured machine.
    let disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    src.attach_ahci_disk_port0(Box::new(disk)).unwrap();
    // Attach a dummy disk backend to the IDE primary master (optional disk_id=2 slot).
    let ide_disk = RawDisk::create(MemBackend::new(), 4 * SECTOR_SIZE as u64).unwrap();
    src.attach_ide_primary_master_disk(Box::new(ide_disk))
        .unwrap();

    // Simulate a configured storage topology by setting overlay references for the canonical disk
    // slots. (Actual disk contents/backends are external to snapshots.)
    src.set_ahci_port0_disk_overlay_ref("hdd.base.img", "hdd.overlay.img");
    src.set_ide_secondary_master_atapi_overlay_ref("install.iso", "install.overlay");
    src.set_ide_primary_master_ata_overlay_ref("ide.base.img", "ide.overlay.img");

    // SnapshotSource::disk_overlays should be deterministic and ordered by disk_id.
    use snapshot::SnapshotSource as _;
    let expected = snapshot::DiskOverlayRefs {
        disks: vec![
            snapshot::DiskOverlayRef {
                disk_id: Machine::DISK_ID_PRIMARY_HDD,
                base_image: "hdd.base.img".to_string(),
                overlay_image: "hdd.overlay.img".to_string(),
            },
            snapshot::DiskOverlayRef {
                disk_id: Machine::DISK_ID_INSTALL_MEDIA,
                base_image: "install.iso".to_string(),
                overlay_image: "install.overlay".to_string(),
            },
            snapshot::DiskOverlayRef {
                disk_id: Machine::DISK_ID_IDE_PRIMARY_MASTER,
                base_image: "ide.base.img".to_string(),
                overlay_image: "ide.overlay.img".to_string(),
            },
        ],
    };
    assert_eq!(src.disk_overlays(), expected);

    let snap = src.take_snapshot_full().unwrap();

    // Decode the snapshot and confirm the DISKS section was populated as expected.
    let mut capture = CaptureDiskOverlaysTarget::new(cfg.ram_size_bytes as usize);
    snapshot::restore_snapshot(&mut std::io::Cursor::new(&snap), &mut capture).unwrap();
    let captured = capture
        .disks
        .expect("snapshot restore target did not receive DISKS section");
    assert_eq!(captured, expected);

    // DISKS entries must be strictly increasing by disk_id (sorted + deduped).
    let disk_ids: Vec<u32> = captured.disks.iter().map(|d| d.disk_id).collect();
    assert!(disk_ids.windows(2).all(|w| w[0] < w[1]));

    // Restore into a real machine and ensure the overlay refs were recorded for host reattach.
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // The machine should expose the restored refs for post-restore host reattachment.
    assert_eq!(restored.restored_disk_overlays(), Some(&expected));
    assert_eq!(
        restored.take_restored_disk_overlays(),
        Some(expected.clone())
    );

    // Disk overlay config should also be reflected in subsequent snapshots.
    assert_eq!(restored.disk_overlays(), expected);
}

#[test]
fn machine_snapshot_restores_with_placeholder_disk_overlay_refs_when_unconfigured() {
    // `Machine::disk_overlays` always emits stable entries for disk_id 0/1 even when the host has
    // not configured any overlay refs yet; empty strings mean "no backend configured".
    //
    // This test locks that contract for the canonical Win7 storage machine.
    let cfg = MachineConfig::win7_storage(2 * 1024 * 1024);

    let mut src = Machine::new(cfg.clone()).unwrap();

    use snapshot::SnapshotSource as _;
    let expected = snapshot::DiskOverlayRefs {
        disks: vec![
            snapshot::DiskOverlayRef {
                disk_id: Machine::DISK_ID_PRIMARY_HDD,
                base_image: String::new(),
                overlay_image: String::new(),
            },
            snapshot::DiskOverlayRef {
                disk_id: Machine::DISK_ID_INSTALL_MEDIA,
                base_image: String::new(),
                overlay_image: String::new(),
            },
        ],
    };

    // The source should expose a deterministic, canonical ordering.
    assert_eq!(src.disk_overlays(), expected);

    let snap = src.take_snapshot_full().unwrap();

    // Decode the snapshot and confirm the DISKS section was populated as expected (including
    // empty-string placeholders).
    let mut capture = CaptureDiskOverlaysTarget::new(cfg.ram_size_bytes as usize);
    snapshot::restore_snapshot(&mut std::io::Cursor::new(&snap), &mut capture).unwrap();
    let captured = capture
        .disks
        .expect("snapshot restore target did not receive DISKS section");
    assert_eq!(captured, expected);

    // DISKS entries must be strictly increasing by disk_id (sorted + deduped).
    let disk_ids: Vec<u32> = captured.disks.iter().map(|d| d.disk_id).collect();
    assert!(disk_ids.windows(2).all(|w| w[0] < w[1]));

    // Restore into a real machine and ensure the overlay refs were recorded for host reattach.
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    assert_eq!(restored.restored_disk_overlays(), Some(&expected));
    assert_eq!(
        restored.take_restored_disk_overlays(),
        Some(expected.clone())
    );

    // Disk overlay config should also be reflected in subsequent snapshots.
    assert_eq!(restored.disk_overlays(), expected);
}
