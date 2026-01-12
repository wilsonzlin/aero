#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile;
use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_io_snapshot::io::storage::state::DiskControllersSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;

fn parse_io_snapshot_version(bytes: &[u8]) -> (u16, u16) {
    assert!(
        bytes.len() >= 16,
        "io-snapshot blob must include the 16-byte header"
    );
    let major = u16::from_le_bytes([bytes[12], bytes[13]]);
    let minor = u16::from_le_bytes([bytes[14], bytes[15]]);
    (major, minor)
}

#[test]
fn machine_restore_accepts_legacy_single_disk_controller_blob_nvme() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the machine minimal and deterministic for a focused snapshot test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let src = Machine::new(cfg.clone()).unwrap();
    let src_nvme = src.nvme().expect("NVMe enabled");
    // Mutate NVMe controller state so restore has observable work to do.
    src_nvme
        .borrow_mut()
        .controller
        .mmio_write(0x000c, 4, 0x1234_5678);

    let states = snapshot::SnapshotSource::device_states(&src);
    let dskc = states
        .iter()
        .find(|s| s.id == snapshot::DeviceId::DISK_CONTROLLER)
        .expect("expected DISK_CONTROLLER entry when NVMe is enabled")
        .clone();

    let mut wrapper = DiskControllersSnapshot::default();
    snapshot::apply_io_snapshot_to_device(&dskc, &mut wrapper).unwrap();

    let nvme_blob = wrapper
        .controllers()
        .get(&profile::NVME_CONTROLLER.bdf.pack_u16())
        .expect("DSKC wrapper missing NVMe entry")
        .clone();
    assert_eq!(nvme_blob.get(8..12), Some(b"NVMP".as_slice()));
    let (major, minor) = parse_io_snapshot_version(&nvme_blob);

    // Rewrite device states to a legacy encoding: store the controller directly under
    // `DeviceId::DISK_CONTROLLER` without the DSKC wrapper.
    let mut legacy: Vec<snapshot::DeviceState> = states
        .into_iter()
        .filter(|s| s.id != snapshot::DeviceId::DISK_CONTROLLER)
        .collect();
    legacy.push(snapshot::DeviceState {
        id: snapshot::DeviceId::DISK_CONTROLLER,
        version: major,
        flags: minor,
        data: nvme_blob.clone(),
    });

    let mut restored = Machine::new(cfg).unwrap();
    let before = restored
        .nvme()
        .expect("NVMe enabled")
        .borrow()
        .save_state();
    assert_ne!(
        before, nvme_blob,
        "precondition: restored machine should start with different NVMe state"
    );

    snapshot::SnapshotTarget::restore_device_states(&mut restored, legacy);
    assert_eq!(
        restored.nvme().unwrap().borrow().save_state(),
        nvme_blob,
        "legacy NVMP blob should be applied to the NVMe device model"
    );
}

#[test]
fn machine_restore_accepts_legacy_single_disk_controller_blob_virtio_blk() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the machine minimal and deterministic for a focused snapshot test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let src = Machine::new(cfg.clone()).unwrap();
    let virtio_blk = src.virtio_blk().expect("virtio-blk enabled");
    // Mutate transport state (device_status) so restore has observable work to do.
    virtio_blk.borrow_mut().bar0_write(0x14, &[1]);

    let states = snapshot::SnapshotSource::device_states(&src);
    let dskc = states
        .iter()
        .find(|s| s.id == snapshot::DeviceId::DISK_CONTROLLER)
        .expect("expected DISK_CONTROLLER entry when virtio-blk is enabled")
        .clone();

    let mut wrapper = DiskControllersSnapshot::default();
    snapshot::apply_io_snapshot_to_device(&dskc, &mut wrapper).unwrap();

    let vblk_blob = wrapper
        .controllers()
        .get(&profile::VIRTIO_BLK.bdf.pack_u16())
        .expect("DSKC wrapper missing virtio-blk entry")
        .clone();
    assert_eq!(vblk_blob.get(8..12), Some(b"VPCI".as_slice()));
    let (major, minor) = parse_io_snapshot_version(&vblk_blob);

    let mut legacy: Vec<snapshot::DeviceState> = states
        .into_iter()
        .filter(|s| s.id != snapshot::DeviceId::DISK_CONTROLLER)
        .collect();
    legacy.push(snapshot::DeviceState {
        id: snapshot::DeviceId::DISK_CONTROLLER,
        version: major,
        flags: minor,
        data: vblk_blob.clone(),
    });

    let mut restored = Machine::new(cfg).unwrap();
    let before = restored
        .virtio_blk()
        .expect("virtio-blk enabled")
        .borrow()
        .save_state();
    assert_ne!(
        before, vblk_blob,
        "precondition: restored machine should start with different virtio-blk state"
    );

    snapshot::SnapshotTarget::restore_device_states(&mut restored, legacy);
    assert_eq!(
        restored.virtio_blk().unwrap().borrow().save_state(),
        vblk_blob,
        "legacy VPCI blob should be applied to the virtio-blk device model"
    );
}
