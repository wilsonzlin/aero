#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile;
use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_io_snapshot::io::storage::state::DiskControllersSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;

#[test]
fn machine_snapshots_nvme_controller_in_dskc_wrapper_and_restores_state() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep this test focused and deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_vga: false,
        enable_e1000: false,
        ..Default::default()
    };

    let src = Machine::new(cfg.clone()).unwrap();
    let states = snapshot::SnapshotSource::device_states(&src);

    let disk_controller_state = states
        .iter()
        .find(|s| s.id == snapshot::DeviceId::DISK_CONTROLLER)
        .expect("expected DISK_CONTROLLER state when NVMe is enabled")
        .clone();

    assert_eq!(
        disk_controller_state.data.get(8..12),
        Some(b"DSKC".as_slice()),
        "expected machine to wrap disk controllers using DSKC"
    );

    let mut wrapper = DiskControllersSnapshot::default();
    snapshot::apply_io_snapshot_to_device(&disk_controller_state, &mut wrapper).unwrap();

    let nvme_key = profile::NVME_CONTROLLER.bdf.pack_u16();
    let nvme_state = wrapper
        .controllers()
        .get(&nvme_key)
        .expect("missing NVMe controller entry for canonical BDF")
        .clone();
    assert_eq!(nvme_state.get(8..12), Some(b"NVMP".as_slice()));

    // Restore into a fresh machine and ensure the NVMe controller state matches the nested blob.
    let mut restored = Machine::new(cfg).unwrap();
    snapshot::SnapshotTarget::restore_device_states(&mut restored, states);

    let nvme = restored.nvme().expect("NVMe enabled");
    assert_eq!(nvme.borrow().save_state(), nvme_state);
}
