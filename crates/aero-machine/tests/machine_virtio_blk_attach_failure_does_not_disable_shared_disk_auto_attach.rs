#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::PciDevice as _;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk};

#[test]
fn virtio_blk_attach_failure_does_not_disable_shared_disk_auto_attach() {
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
    {
        // Ensure BAR0 reads are not gated by PCI command register state.
        let command = virtio_blk.borrow().config().command();
        virtio_blk
            .borrow_mut()
            .set_pci_command(command | (1 << 1) | (1 << 2));
    }
    let bar0_base = virtio_blk
        .borrow()
        .config()
        .bar_range(0)
        .expect("virtio-blk should have BAR0")
        .base;
    assert_ne!(bar0_base, 0, "expected virtio-blk BAR0 to be assigned");

    // Virtio-pci device-specific config lives at BAR0 + 0x3000.
    const DEVICE_CFG_OFFSET: u64 = 0x3000;

    let mut cap_bytes = [0u8; 8];
    virtio_blk
        .borrow_mut()
        .bar0_read(DEVICE_CFG_OFFSET, &mut cap_bytes);
    let initial_capacity_sectors = u64::from_le_bytes(cap_bytes);

    // Attach a disk with an invalid (non-512-aligned) capacity; this should fail.
    let bad_disk = RawDisk::create(MemBackend::new(), 513).unwrap();
    assert!(m.attach_virtio_blk_disk(Box::new(bad_disk)).is_err());

    // If the failed attach incorrectly disabled shared-disk auto-attach, the virtio-blk capacity
    // config would remain stale after changing the shared disk backend.
    let good_disk = RawDisk::create(MemBackend::new(), 2 * 512).unwrap();
    m.set_disk_backend(Box::new(good_disk)).unwrap();

    virtio_blk
        .borrow_mut()
        .bar0_read(DEVICE_CFG_OFFSET, &mut cap_bytes);
    let new_capacity_sectors = u64::from_le_bytes(cap_bytes);

    assert_ne!(
        new_capacity_sectors,
        initial_capacity_sectors,
        "expected virtio-blk capacity to update after shared disk backend change"
    );
    assert_eq!(
        new_capacity_sectors, 2,
        "expected virtio-blk to report the new shared disk capacity"
    );
}
