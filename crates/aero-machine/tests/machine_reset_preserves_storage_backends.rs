#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_devices_storage::ata::AtaDrive;
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
fn machine_reset_does_not_drop_shared_disk_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        // Keep the machine minimal for deterministic reset behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    m.set_disk_backend(Box::new(disk)).unwrap();

    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the shared disk backend"
    );

    // Dropping the machine should drop the backend (sanity check that it was attached).
    drop(m);
    assert!(
        dropped.load(Ordering::SeqCst),
        "dropping the machine should drop the shared disk backend"
    );
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

    m.attach_virtio_blk_disk(Box::new(disk))
        .expect("attaching virtio-blk disk should succeed");

    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the attached virtio-blk backend"
    );

    // Replacing the device should drop the previous backend (sanity check that it was attached).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_virtio_blk_disk(Box::new(replacement))
        .expect("attaching virtio-blk disk should succeed");
    assert!(
        dropped.load(Ordering::SeqCst),
        "replacing the virtio-blk device should drop the previous disk backend"
    );
}

#[test]
fn set_disk_backend_does_not_clobber_custom_virtio_blk_backend() {
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

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    // Explicitly attach a custom virtio-blk backend so the machine should not overwrite it when
    // the canonical SharedDisk backend changes (e.g. via `set_disk_backend`).
    m.attach_virtio_blk_disk(Box::new(disk))
        .expect("attaching virtio-blk disk should succeed");

    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.set_disk_backend(Box::new(replacement)).unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "set_disk_backend dropped/replaced the custom virtio-blk backend"
    );

    // Reset should also preserve the explicitly attached backend.
    m.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the custom virtio-blk backend"
    );

    // Re-attaching the shared disk should drop the custom backend (sanity check).
    m.attach_shared_disk_to_virtio_blk()
        .expect("attaching shared disk to virtio-blk should succeed");
    assert!(
        dropped.load(Ordering::SeqCst),
        "re-attaching SharedDisk should drop the previous custom virtio-blk backend"
    );
}

#[test]
fn set_disk_image_does_not_clobber_custom_virtio_blk_backend() {
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

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    // Explicitly attach a custom virtio-blk backend so the machine should not overwrite it when
    // the canonical SharedDisk image changes (e.g. via `set_disk_image`).
    m.attach_virtio_blk_disk(Box::new(disk))
        .expect("attaching virtio-blk disk should succeed");

    m.set_disk_image(vec![0u8; SECTOR_SIZE]).unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "set_disk_image dropped/replaced the custom virtio-blk backend"
    );

    // Reset should also preserve the explicitly attached backend.
    m.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the custom virtio-blk backend"
    );

    // Re-attaching the shared disk should drop the custom backend (sanity check).
    m.attach_shared_disk_to_virtio_blk()
        .expect("attaching shared disk to virtio-blk should succeed");
    assert!(
        dropped.load(Ordering::SeqCst),
        "re-attaching SharedDisk should drop the previous custom virtio-blk backend"
    );
}

#[test]
fn set_disk_image_does_not_clobber_custom_ahci_port0_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the machine minimal for deterministic reset behavior.
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
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

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    // Explicitly attach a custom disk backend to AHCI port 0 so the machine should not overwrite
    // it when the canonical SharedDisk backend changes (e.g. via `set_disk_image`).
    m.attach_ahci_disk_port0(Box::new(disk))
        .expect("attaching AHCI port0 disk should succeed");

    m.set_disk_image(vec![0u8; SECTOR_SIZE]).unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "set_disk_image dropped/replaced the custom AHCI port0 backend"
    );

    m.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the custom AHCI port0 backend"
    );

    // Re-attaching the shared disk should drop the custom backend (sanity check).
    m.attach_shared_disk_to_ahci_port0().unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "re-attaching SharedDisk should drop the previous custom AHCI backend"
    );
}

#[test]
fn set_disk_backend_does_not_clobber_custom_ahci_port0_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the machine minimal for deterministic reset behavior.
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
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

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    // Explicitly attach a custom disk backend to AHCI port 0 so the machine should not overwrite
    // it when the canonical SharedDisk backend changes (e.g. via `set_disk_backend`).
    m.attach_ahci_disk_port0(Box::new(disk))
        .expect("attaching AHCI port0 disk should succeed");

    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.set_disk_backend(Box::new(replacement)).unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "set_disk_backend dropped/replaced the custom AHCI port0 backend"
    );

    m.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the custom AHCI port0 backend"
    );

    // Re-attaching the shared disk should drop the custom backend (sanity check).
    m.attach_shared_disk_to_ahci_port0().unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "re-attaching SharedDisk should drop the previous custom AHCI backend"
    );
}

#[test]
fn set_disk_image_does_not_clobber_custom_nvme_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        enable_ahci: false,
        enable_ide: false,
        enable_virtio_blk: false,
        // Keep the machine minimal for deterministic reset behavior.
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

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    // Explicitly attach a custom NVMe disk backend. Updates to the machine's canonical shared disk
    // (via `set_disk_image`) should not overwrite it.
    m.attach_nvme_disk(Box::new(disk)).unwrap();

    m.set_disk_image(vec![0u8; SECTOR_SIZE]).unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "set_disk_image dropped/replaced the custom NVMe backend"
    );

    // Reset should preserve the host-attached NVMe backend.
    m.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the custom NVMe backend"
    );
}

#[test]
fn set_disk_backend_does_not_clobber_custom_nvme_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        enable_ahci: false,
        enable_ide: false,
        enable_virtio_blk: false,
        // Keep the machine minimal for deterministic reset behavior.
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

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    // Explicitly attach a custom NVMe disk backend. Updates to the machine's canonical shared disk
    // (via `set_disk_backend`) should not overwrite it.
    m.attach_nvme_disk(Box::new(disk)).unwrap();

    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.set_disk_backend(Box::new(replacement)).unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "set_disk_backend dropped/replaced the custom NVMe backend"
    );

    // Reset should preserve the host-attached NVMe backend.
    m.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the custom NVMe backend"
    );
}

#[test]
fn machine_reset_does_not_detach_nvme_disk_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        enable_ahci: false,
        enable_ide: false,
        enable_virtio_blk: false,
        // Keep the machine minimal for deterministic reset behavior.
        enable_uhci: false,
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

    m.attach_nvme_disk(Box::new(disk)).unwrap();
    m.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "machine reset dropped the attached NVMe disk backend"
    );

    // Replacing the disk should drop the previous backend (sanity check that it was attached).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_nvme_disk(Box::new(replacement)).unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "replacing the NVMe disk should drop the previous backend"
    );
}
