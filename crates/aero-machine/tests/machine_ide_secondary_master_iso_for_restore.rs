#![cfg(not(target_arch = "wasm32"))]

use aero_devices_storage::atapi::AtapiCdrom;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk};

#[test]
fn machine_attach_ide_secondary_master_iso_for_restore_returns_ok_after_snapshot_restore() {
    let mut cfg = MachineConfig::win7_storage_defaults(2 * 1024 * 1024);
    // Keep the test focused on IDE + ISO reattachment.
    cfg.enable_serial = false;
    cfg.enable_i8042 = false;
    cfg.enable_a20_gate = false;
    cfg.enable_reset_ctrl = false;
    cfg.enable_vga = false;

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Attach a tiny ISO backend (2048-byte sectors).
    let iso = RawDisk::create(
        MemBackend::new(),
        4 * AtapiCdrom::SECTOR_SIZE as u64, // 4 sectors
    )
    .unwrap();
    src.attach_ide_secondary_master_iso(Box::new(iso)).unwrap();

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Snapshot restore dropped the host ISO backend; reattach it via the new helper.
    let iso = RawDisk::create(
        MemBackend::new(),
        4 * AtapiCdrom::SECTOR_SIZE as u64, // 4 sectors
    )
    .unwrap();
    assert!(restored
        .attach_ide_secondary_master_iso_for_restore(Box::new(iso))
        .is_ok());
}
