#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_devices_storage::ata::AtaDrive;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use aero_virtio::devices::blk::VirtioBlk;
use aero_virtio::pci::{InterruptSink, VirtioPciDevice};

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

#[derive(Debug, Default)]
struct NoopVirtioInterruptSink;

impl InterruptSink for NoopVirtioInterruptSink {
    fn raise_legacy_irq(&mut self) {}
    fn lower_legacy_irq(&mut self) {}
    fn signal_msix(&mut self, _vector: u16) {}
}

#[test]
fn machine_reset_does_not_detach_ahci_disk_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the machine minimal for deterministic reset behavior.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    m.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the attached disk backend"
    );

    // Detaching the drive should drop the backend (sanity check that we were actually attached).
    m.detach_ahci_drive_port0();
    assert!(
        dropped.load(Ordering::SeqCst),
        "detaching the drive should drop the disk backend"
    );
}

#[test]
fn machine_reset_does_not_detach_ide_disk_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ide: true,
        enable_ahci: false,
        // Keep the machine minimal for deterministic reset behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    m.attach_ide_primary_master_drive(AtaDrive::new(Box::new(disk)).unwrap());
    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the attached IDE disk backend"
    );

    // Replacing the drive should drop the previous backend (sanity check that it was attached).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_primary_master_drive(AtaDrive::new(Box::new(replacement)).unwrap());
    assert!(
        dropped.load(Ordering::SeqCst),
        "replacing the IDE drive should drop the previous disk backend"
    );
}

#[test]
fn machine_reset_does_not_detach_ide_secondary_master_iso_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ide: true,
        enable_ahci: false,
        // Keep the machine minimal for deterministic reset behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let dropped = Arc::new(AtomicBool::new(false));
    // ATAPI uses 2048-byte sectors, so ensure capacity is 2048-aligned.
    let capacity = 16 * SECTOR_SIZE as u64;
    assert_eq!(capacity % 2048, 0);
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    m.attach_ide_secondary_master_iso(Box::new(disk)).unwrap();

    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the attached IDE ISO backend"
    );

    // Replacing the ISO should drop the previous backend (sanity check that it was attached).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_secondary_master_iso(Box::new(replacement))
        .unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "replacing the IDE ISO should drop the previous backend"
    );
}

#[test]
fn machine_reset_does_not_detach_virtio_blk_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the machine minimal for deterministic reset behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    let virtio_blk = m.virtio_blk().expect("virtio-blk should be enabled");
    *virtio_blk.borrow_mut() = VirtioPciDevice::new(
        Box::new(VirtioBlk::new(Box::new(disk))),
        Box::new(NoopVirtioInterruptSink),
    );

    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the attached virtio-blk backend"
    );

    // Replacing the device should drop the previous backend (sanity check that it was attached).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    *virtio_blk.borrow_mut() = VirtioPciDevice::new(
        Box::new(VirtioBlk::new(Box::new(replacement))),
        Box::new(NoopVirtioInterruptSink),
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "replacing the virtio-blk device should drop the previous disk backend"
    );
}
