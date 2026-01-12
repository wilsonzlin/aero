#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_machine::pc::PcMachine;
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
fn pc_machine_reset_does_not_detach_ahci_disk_backend() {
    let mut m = PcMachine::new(2 * 1024 * 1024);

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    m.bus
        .platform
        .attach_ahci_disk_port0(Box::new(disk))
        .unwrap();

    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the attached disk backend"
    );
}
