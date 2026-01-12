#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_devices::pci::PciDevice as _;
use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use aero_virtio::pci::{VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_STATUS_ACKNOWLEDGE};

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

fn virtio_common_cfg_offset(cfg: &[u8; 256]) -> u64 {
    // `PciConfigSpace::snapshot_state()` returns standard 256-byte config space.
    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        let cap_id = cfg[ptr];
        let next = cfg[ptr + 1] as usize;

        // Vendor-specific capability.
        if cap_id == 0x09 {
            let cfg_type = cfg[ptr + 3];
            if cfg_type == VIRTIO_PCI_CAP_COMMON_CFG {
                let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
                return offset;
            }
        }

        ptr = next;
    }

    panic!("missing virtio-pci common config capability");
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

    let cfg_bytes = virtio_blk.borrow().config().snapshot_state().bytes;
    let common = virtio_common_cfg_offset(&cfg_bytes);

    // Mutate some non-default transport state so a naive "recreate device" backend swap would
    // wipe it.
    virtio_blk
        .borrow_mut()
        .bar0_write(common + 0x14, &[VIRTIO_STATUS_ACKNOWLEDGE]);

    let state_before = virtio_blk.borrow().save_state();

    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    m.attach_virtio_blk_disk(Box::new(disk))
        .expect("attaching virtio-blk disk should succeed");

    // Attaching the disk backend should preserve controller state by snapshotting and restoring.
    assert_eq!(virtio_blk.borrow().save_state(), state_before);

    // Reset should preserve the attached disk backend (virtio-blk reset does not swap disks).
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
