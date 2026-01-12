#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use aero_virtio::devices::blk::VirtioBlk;
use aero_virtio::pci::{InterruptLog, VirtioPciDevice};

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
fn machine_reset_does_not_detach_virtio_blk_disk_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the machine minimal for deterministic reset behavior.
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

    // Replace the default in-memory backend with a drop-detecting `aero-storage` disk so we can
    // assert that `Machine::reset()` does not silently drop/replace the backend.
    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    {
        let mut dev = virtio_blk.borrow_mut();
        *dev = VirtioPciDevice::new(
            Box::new(VirtioBlk::new(Box::new(disk))),
            Box::new(InterruptLog::default()),
        );
    }

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

