#![cfg(not(target_arch = "wasm32"))]

use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};

#[test]
fn machine_set_disk_backend_preserves_virtio_blk_state() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the machine minimal and deterministic for a focused test.
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_uhci: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    let virtio_blk = m.virtio_blk().expect("virtio-blk should be enabled");

    // Mutate transport state so the snapshot blob is non-default.
    virtio_blk.borrow_mut().bar0_write(0x14, &[1]);
    let state_before = virtio_blk.borrow().save_state();

    // Replacing the machine's canonical disk backend should not reset virtio-blk transport state.
    // This is particularly important for snapshot restore flows, where the host reattaches the
    // disk backend after restoring device state.
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.set_disk_backend(Box::new(disk)).unwrap();

    assert_eq!(virtio_blk.borrow().save_state(), state_before);
}
