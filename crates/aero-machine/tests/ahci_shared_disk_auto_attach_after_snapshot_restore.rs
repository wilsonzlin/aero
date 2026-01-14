#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};

#[test]
fn ahci_shared_disk_auto_attach_survives_snapshot_restore() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the machine minimal for deterministic snapshot behaviour.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).expect("Machine::new should succeed");
    let snap = src.take_snapshot_full().expect("snapshot should succeed");

    let mut restored = Machine::new(cfg).expect("Machine::new should succeed");
    restored
        .restore_snapshot_bytes(&snap)
        .expect("restore should succeed");

    let ahci = restored.ahci().expect("AHCI should be enabled");
    assert!(
        !ahci.borrow().drive_attached(0),
        "snapshot restore should drop host-side AHCI drive backends"
    );

    // Reattach disk bytes using the canonical shared-disk API. This is the pattern used by the
    // browser/WASM snapshot reattach helper (`Machine::reattach_restored_disks_from_opfs`) and
    // native hosts that reopen disk backends out-of-band.
    let disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64)
        .expect("RawDisk::create should succeed");
    restored
        .set_disk_backend(Box::new(disk))
        .expect("set_disk_backend should succeed");

    assert!(
        ahci.borrow().drive_attached(0),
        "expected set_disk_backend() to reattach the shared disk to AHCI port0 when auto-attach is enabled"
    );
}
