#![cfg(all(feature = "io-snapshot", not(target_arch = "wasm32")))]

use aero_net_e1000::{E1000Device, E1000_IO_SIZE};
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::DeviceId;

#[test]
fn e1000_io_snapshot_roundtrips_through_bridge() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

    // Program BAR0 and leave BAR1 in probe mode.
    dev.pci_config_write(0x10, 4, 0xDEAD_BEEF);
    dev.pci_config_write(0x14, 4, 0xFFFF_FFFF);

    // Select an IOADDR register.
    dev.io_write(0x0, 4, 0x1234);

    // Some representative MMIO state (ring pointers + interrupts + one "other" register).
    dev.mmio_write_u32(0x2800, 0x1111_0000); // RDBAL
    dev.mmio_write_u32(0x2804, 0x2222_0000); // RDBAH
    // Make the ring length valid so restore doesn't clamp indices back to 0.
    dev.mmio_write_u32(0x2808, 16 * 256); // RDLEN (16-byte descriptors)
    dev.mmio_write_u32(0x2810, 0x33); // RDH
    dev.mmio_write_u32(0x2818, 0x44); // RDT

    dev.mmio_write_u32(0x00D0, 0xFFFF_FFFF); // IMS
    dev.mmio_write_u32(0x00C8, 0x0000_0080); // ICS: RXT0
    assert!(dev.irq_level());

    dev.mmio_write_u32(0x1234, 0xCAFE_BABE);

    // Program the MAC address via RAL0/RAH0.
    dev.mmio_write_u32(0x5400, 0x4433_2211);
    dev.mmio_write_u32(0x5404, 0x8000_6655);

    let state = device_state_from_io_snapshot(DeviceId::E1000, &dev);

    let mut restored = E1000Device::new([0; 6]);
    apply_io_snapshot_to_device(&state, &mut restored).unwrap();

    // BAR0 should be aligned on restore.
    assert_eq!(restored.pci_read_u32(0x10), 0xDEAD_BEE0);
    // BAR1 probe reads return the size mask.
    let expected_bar1 = (!(E1000_IO_SIZE - 1) & 0xFFFF_FFFC) | 0x1;
    assert_eq!(restored.pci_read_u32(0x14), expected_bar1);

    assert_eq!(restored.io_read(0x0, 4), 0x1234);

    assert_eq!(restored.mmio_read_u32(0x2800), 0x1111_0000);
    assert_eq!(restored.mmio_read_u32(0x2804), 0x2222_0000);
    assert_eq!(restored.mmio_read_u32(0x2810), 0x33);
    assert_eq!(restored.mmio_read_u32(0x2818), 0x44);

    assert_eq!(restored.mmio_read_u32(0x00D0), 0xFFFF_FFFF);
    assert!(restored.irq_level());
    let icr = restored.mmio_read_u32(0x00C0);
    assert_ne!(icr & 0x0000_0080, 0);
    assert!(!restored.irq_level());

    assert_eq!(restored.mmio_read_u32(0x1234), 0xCAFE_BABE);
    assert_eq!(restored.mmio_read_u32(0x5400), 0x4433_2211);
    assert_eq!(restored.mmio_read_u32(0x5404), 0x8000_6655);
}
