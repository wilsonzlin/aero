#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};

struct DropDetectDisk {
    inner: RawDisk<MemBackend>,
    dropped: Arc<AtomicBool>,
}

impl Drop for DropDetectDisk {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl VirtualDisk for DropDetectDisk {
    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.flush()
    }
}

#[test]
fn machine_attach_virtio_blk_disk_preserves_state_and_survives_reset() {
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

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    m.attach_virtio_blk_disk(Box::new(disk));

    // Attaching the disk backend should preserve controller state by snapshotting and restoring.
    assert_eq!(virtio_blk.borrow().save_state(), state_before);

    // Reset should preserve the attached disk backend.
    m.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the virtio-blk disk backend"
    );

    // Dropping the machine should drop the backend (sanity check that we were actually attached).
    drop(virtio_blk);
    drop(m);
    assert!(
        dropped.load(Ordering::SeqCst),
        "dropping the machine should drop the virtio-blk disk backend"
    );
}
