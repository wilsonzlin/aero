use aero_virtio::devices::input::{VirtioInput, VirtioInputEvent};
use aero_virtio::memory::{
    read_u16_le, read_u32_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam,
};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, PCI_VENDOR_ID_VIRTIO, VIRTIO_PCI_CAP_COMMON_CFG,
    VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_WRITE;

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    isr: u64,
    device: u64,
    notify_mult: u32,
}

fn parse_caps(dev: &VirtioPciDevice) -> Caps {
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut caps = Caps::default();

    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        assert_eq!(cfg[ptr], 0x09);
        let next = cfg[ptr + 1] as usize;
        let cfg_type = cfg[ptr + 3];
        let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
        match cfg_type {
            VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
            VIRTIO_PCI_CAP_NOTIFY_CFG => {
                caps.notify = offset;
                caps.notify_mult = u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
            }
            VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
            VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
            _ => {}
        }
        ptr = next;
    }

    caps
}

fn bar_read_u32(dev: &mut VirtioPciDevice, off: u64) -> u32 {
    let mut buf = [0u8; 4];
    dev.bar0_read(off, &mut buf);
    u32::from_le_bytes(buf)
}

fn bar_write_u32(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u32) {
    dev.bar0_write(off, &val.to_le_bytes(), mem);
}

fn bar_write_u16(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes(), mem);
}

fn bar_write_u64(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u64) {
    dev.bar0_write(off, &val.to_le_bytes(), mem);
}

fn bar_write_u8(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u8) {
    dev.bar0_write(off, &[val], mem);
}

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

#[test]
fn virtio_input_posts_buffers_then_delivers_events() {
    let input = VirtioInput::new();
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));

    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    let caps = parse_caps(&dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation.
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x00, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x00, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f1);

    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure event queue 0.
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 0);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    let event_buf = 0x4000;
    mem.write(event_buf, &[0u8; 8]).unwrap();
    write_desc(&mut mem, desc, 0, event_buf, 8, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + 0 * u64::from(caps.notify_mult),
        &0u16.to_le_bytes(),
        &mut mem,
    );
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 0);

    // Host injects an input event.
    dev.device_mut::<VirtioInput>()
        .unwrap()
        .push_event(VirtioInputEvent {
            type_: 1,
            code: 30,
            value: 1,
        });
    dev.poll(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 8);
    assert_eq!(
        mem.get_slice(event_buf, 8).unwrap(),
        &[1, 0, 30, 0, 1, 0, 0, 0]
    );
}

#[test]
fn virtio_input_statusq_does_not_stall_queue_on_device_error() {
    let input = VirtioInput::new();
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));

    let caps = parse_caps(&dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation (mirrors the render path tests).
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x00, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x00, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f1);

    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure status queue 1. The current VirtioInput device model does not implement statusq
    // semantics; the virtio-pci transport must still complete the chain to avoid wedging the queue.
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    let buf = 0x8000;
    mem.write(buf, &[0u8; 4]).unwrap();
    write_desc(&mut mem, desc, 0, buf, 4, 0, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + 1 * u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
        &mut mem,
    );

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);
}
