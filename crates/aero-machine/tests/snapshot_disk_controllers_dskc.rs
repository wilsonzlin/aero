#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciDevice as _};
use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_io_snapshot::io::storage::state::DiskControllersSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot::io_snapshot_bridge::apply_io_snapshot_to_device;
use aero_snapshot::DeviceId;

#[test]
fn snapshot_dskc_no_collision_with_multiple_controllers() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_nvme: true,
        enable_virtio_blk: true,
        // Keep the snapshot small and deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_vga: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Regression guard: historically, AHCI (`AHCP` 1.0) + virtio-pci (`VPCI` 1.0) would collide when
    // both were stored as outer `DeviceId::DISK_CONTROLLER` entries.
    m.take_snapshot_full().expect("snapshot should succeed");

    let devices = aero_snapshot::SnapshotSource::device_states(&m);
    let disk_entries: Vec<_> = devices
        .iter()
        .filter(|d| d.id == DeviceId::DISK_CONTROLLER)
        .collect();
    assert_eq!(
        disk_entries.len(),
        1,
        "expected exactly one DISK_CONTROLLER entry"
    );

    let state = disk_entries[0];
    let inner_id: [u8; 4] = state
        .data
        .get(8..12)
        .unwrap_or(&[])
        .try_into()
        .unwrap_or([0u8; 4]);
    assert_eq!(inner_id, *b"DSKC", "expected DSKC wrapper payload");

    let mut wrapper = DiskControllersSnapshot::default();
    apply_io_snapshot_to_device(state, &mut wrapper).expect("DSKC decode should succeed");

    assert!(
        wrapper
            .controllers()
            .contains_key(&profile::SATA_AHCI_ICH9.bdf.pack_u16()),
        "expected AHCI controller entry in DSKC wrapper"
    );
    assert!(
        wrapper
            .controllers()
            .contains_key(&profile::NVME_CONTROLLER.bdf.pack_u16()),
        "expected NVMe controller entry in DSKC wrapper"
    );
    assert!(
        wrapper
            .controllers()
            .contains_key(&profile::VIRTIO_BLK.bdf.pack_u16()),
        "expected virtio-blk controller entry in DSKC wrapper"
    );
}

#[test]
fn snapshot_restore_roundtrip_preserves_disk_controller_device_state_bytes() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_virtio_blk: true,
        // Keep the snapshot small and deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_vga: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let ahci = m.ahci().expect("AHCI should be enabled");
    let virtio_blk = m.virtio_blk().expect("virtio-blk should be enabled");

    let ahci_before = ahci.borrow().save_state();
    let virtio_before = virtio_blk.borrow().save_state();

    let snap = m.take_snapshot_full().expect("snapshot should succeed");

    // Mutate controller state so restore has something to do.
    {
        // Flip GHC.IE (bit 1) so the controller state blob changes.
        let mut dev = ahci.borrow_mut();
        dev.config_mut().set_command(0x6); // MEM + BME for MMIO access.
        let ghc = dev.mmio_read(0x04, 4) as u32;
        dev.mmio_write(0x04, 4, u64::from(ghc ^ (1 << 1)));
    }
    {
        // Mutate the virtio PCI config command register so the controller snapshot blob changes.
        virtio_blk.borrow_mut().config_mut().set_command(0x1234);
    }

    assert_ne!(
        ahci.borrow().save_state(),
        ahci_before,
        "expected AHCI state to change before restore"
    );
    assert_ne!(
        virtio_blk.borrow().save_state(),
        virtio_before,
        "expected virtio-blk state to change before restore"
    );

    m.restore_snapshot_bytes(&snap)
        .expect("snapshot restore should succeed");

    assert_eq!(
        ahci.borrow().save_state(),
        ahci_before,
        "AHCI state bytes should roundtrip across snapshot/restore"
    );
    assert_eq!(
        virtio_blk.borrow().save_state(),
        virtio_before,
        "virtio-blk state bytes should roundtrip across snapshot/restore"
    );
}
