use aero_virtio::devices::input::{KEY_A, VirtioInputDeviceKind};
use aero_virtio::memory::{
    GuestMemory, GuestRam, read_u16_le, read_u32_le, write_u16_le, write_u32_le, write_u64_le,
};
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_ISR_QUEUE, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_WRITE;
use aero_wasm::VirtioInputPciDeviceCore;

fn write_desc(
    mem: &mut GuestRam,
    table: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = table + u64::from(index) * 16;
    write_u64_le(mem, base, addr).unwrap();
    write_u32_le(mem, base + 8, len).unwrap();
    write_u16_le(mem, base + 12, flags).unwrap();
    write_u16_le(mem, base + 14, next).unwrap();
}

fn mmio_read_u32(dev: &mut VirtioInputPciDeviceCore, off: u64) -> u32 {
    dev.mmio_read(off, 4)
}

fn mmio_read_u8(dev: &mut VirtioInputPciDeviceCore, off: u64) -> u8 {
    dev.mmio_read(off, 1) as u8
}

fn mmio_write_u32(dev: &mut VirtioInputPciDeviceCore, mem: &mut GuestRam, off: u64, val: u32) {
    dev.mmio_write(off, 4, val, mem);
}

fn mmio_write_u16(dev: &mut VirtioInputPciDeviceCore, mem: &mut GuestRam, off: u64, val: u16) {
    dev.mmio_write(off, 2, u32::from(val), mem);
}

fn mmio_write_u8(dev: &mut VirtioInputPciDeviceCore, mem: &mut GuestRam, off: u64, val: u8) {
    dev.mmio_write(off, 1, u32::from(val), mem);
}

#[test]
fn virtio_input_pci_device_core_can_handshake_post_event_and_toggle_irq() {
    // BAR0 layout in `aero_virtio::pci::VirtioPciDevice`:
    // - common: 0x0000
    // - notify: 0x1000
    // - isr: 0x2000
    const COMMON: u64 = 0x0000;
    const NOTIFY: u64 = 0x1000;
    const ISR: u64 = 0x2000;

    let mut dev = VirtioInputPciDeviceCore::new(VirtioInputDeviceKind::Keyboard);
    // Allow the device to DMA into guest memory (virtqueue descriptor reads / used writes).
    dev.set_pci_command(0x0004);
    let mut mem = GuestRam::new(0x10000);

    assert!(!dev.driver_ok());

    // Feature negotiation (mirrors `crates/aero-virtio/tests/virtio_input.rs`).
    mmio_write_u8(&mut dev, &mut mem, COMMON + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    mmio_write_u8(
        &mut dev,
        &mut mem,
        COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // Copy all offered features into the driver features bitmap.
    mmio_write_u32(&mut dev, &mut mem, COMMON, 0);
    let f0 = mmio_read_u32(&mut dev, COMMON + 0x04);
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x08, 0);
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x0c, f0);

    mmio_write_u32(&mut dev, &mut mem, COMMON, 1);
    let f1 = mmio_read_u32(&mut dev, COMMON + 0x04);
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x08, 1);
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x0c, f1);

    mmio_write_u8(
        &mut dev,
        &mut mem,
        COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    mmio_write_u8(
        &mut dev,
        &mut mem,
        COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );
    assert!(dev.driver_ok());

    // Configure event queue 0 (eventq).
    let desc = 0x1000u64;
    let avail = 0x2000u64;
    let used = 0x3000u64;
    mmio_write_u16(&mut dev, &mut mem, COMMON + 0x16, 0); // queue_select

    // queue_desc (low/high dword)
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x20, desc as u32);
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x24, 0);
    // queue_avail
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x28, avail as u32);
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x2c, 0);
    // queue_used
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x30, used as u32);
    mmio_write_u32(&mut dev, &mut mem, COMMON + 0x34, 0);
    // queue_enable
    mmio_write_u16(&mut dev, &mut mem, COMMON + 0x1c, 1);

    // Post a single event buffer (8 bytes).
    let event_buf = 0x4000u64;
    mem.write(event_buf, &[0u8; 8]).unwrap();
    write_desc(&mut mem, desc, 0, event_buf, 8, VIRTQ_DESC_F_WRITE, 0);

    // avail: idx=1, ring[0]=0.
    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    // used: idx=0.
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    // Kick queue 0. This should make the buffer available but not raise an IRQ yet.
    mmio_write_u16(&mut dev, &mut mem, NOTIFY, 0);
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 0);
    assert!(!dev.irq_asserted());

    // Host injects a key press. `inject_key` also calls `poll()`.
    dev.inject_key(KEY_A, true, &mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 8);
    assert_eq!(
        mem.get_slice(event_buf, 8).unwrap(),
        &[1, 0, KEY_A as u8, 0, 1, 0, 0, 0]
    );

    assert!(dev.irq_asserted());

    // Guest reads ISR (read-to-clear), which must deassert INTx.
    let isr = mmio_read_u8(&mut dev, ISR);
    assert_eq!(
        isr & VIRTIO_PCI_LEGACY_ISR_QUEUE,
        VIRTIO_PCI_LEGACY_ISR_QUEUE
    );
    assert!(!dev.irq_asserted());
}
