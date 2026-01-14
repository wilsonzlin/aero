use aero_platform::interrupts::msi::MsiMessage;
use aero_virtio::devices::input::{
    VirtioInput, VirtioInputDeviceKind, VirtioInputEvent, ABS_X, ABS_Y, BTN_BACK, BTN_EXTRA,
    BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, BTN_TASK, EV_ABS, EV_KEY, EV_LED,
    EV_REL, EV_SYN, KEY_A, KEY_F1, KEY_F12, KEY_MUTE, KEY_NEXTSONG, KEY_NUMLOCK, KEY_PLAYPAUSE,
    KEY_PREVIOUSSONG, KEY_SCROLLLOCK, KEY_STOPCD, KEY_VOLUMEDOWN, KEY_VOLUMEUP, LED_CAPSL,
    LED_COMPOSE, LED_KANA, LED_NUML, LED_SCROLLL, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, SYN_REPORT,
    VIRTIO_INPUT_CFG_ABS_INFO, VIRTIO_INPUT_CFG_EV_BITS, VIRTIO_INPUT_CFG_ID_DEVIDS,
    VIRTIO_INPUT_CFG_ID_NAME,
};
use aero_virtio::memory::{
    read_u16_le, read_u32_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam,
};
use aero_virtio::pci::{
    InterruptLog, InterruptSink, VirtioPciDevice, PCI_VENDOR_ID_VIRTIO, VIRTIO_F_VERSION_1,
    VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG,
    VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    isr: u64,
    device: u64,
    notify_mult: u32,
}

fn parse_caps(dev: &mut VirtioPciDevice) -> Caps {
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut caps = Caps::default();

    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        let cap_id = cfg[ptr];
        let next = cfg[ptr + 1] as usize;
        if cap_id == 0x09 {
            let cfg_type = cfg[ptr + 3];
            let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    caps.notify = offset;
                    caps.notify_mult =
                        u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
                }
                VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
                VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
                _ => {}
            }
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

fn bar_read_u16(dev: &mut VirtioPciDevice, off: u64) -> u16 {
    let mut buf = [0u8; 2];
    dev.bar0_read(off, &mut buf);
    u16::from_le_bytes(buf)
}

fn bar_read_u8(dev: &mut VirtioPciDevice, off: u64) -> u8 {
    let mut buf = [0u8; 1];
    dev.bar0_read(off, &mut buf);
    buf[0]
}

fn bar_read(dev: &mut VirtioPciDevice, off: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    dev.bar0_read(off, &mut buf);
    buf
}

fn bar_write_u32(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u32) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u16(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u64(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u64) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u8(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u8) {
    dev.bar0_write(off, &[val]);
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

fn input_event_bytes(type_: u16, code: u16, value: i32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&type_.to_le_bytes());
    out[2..4].copy_from_slice(&code.to_le_bytes());
    out[4..8].copy_from_slice(&value.to_le_bytes());
    out
}

#[test]
fn virtio_input_posts_buffers_then_delivers_events() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));

    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);
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
fn virtio_input_statusq_buffers_are_consumed() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));

    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1. Contract v1 requires the device to consume and complete all
    // statusq buffers (it may ignore their contents).
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Win7 virtio-input posts statusq buffers containing packed virtio_input_event entries:
    // 5x EV_LED + final EV_SYN => 48 bytes.
    let buf = 0x8000;
    mem.write(buf, &[0u8; 48]).unwrap();
    write_desc(&mut mem, desc, 0, buf, 48, 0, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);
}

#[test]
fn virtio_input_statusq_led_events_update_mask() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));

    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1 with a single buffer containing multiple virtio_input_event
    // entries (LED updates followed by SYN_REPORT).
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    let buf = 0x8000;
    let payload = [
        input_event_bytes(EV_LED, LED_NUML, 1),
        input_event_bytes(EV_LED, LED_CAPSL, 1),
        input_event_bytes(EV_LED, LED_SCROLLL, 0),
        input_event_bytes(EV_LED, LED_COMPOSE, 1),
        input_event_bytes(EV_LED, LED_KANA, 1),
        input_event_bytes(EV_SYN, SYN_REPORT, 0),
    ]
    .concat();
    mem.write(buf, &payload).unwrap();

    // Intentionally mark the status buffer as write-only for the device; the model should still
    // safely parse and consume it.
    write_desc(
        &mut mem,
        desc,
        0,
        buf,
        payload.len() as u32,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    // The chain should be completed.
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);

    // And the LED state should be updated (bit0=num, bit1=caps, bit2=scroll, bit3=compose, bit4=kana).
    let leds = dev.device_mut::<VirtioInput>().unwrap().leds_mask();
    assert_eq!(leds, 0b11011);
}

#[test]
fn virtio_input_statusq_led_events_split_across_descriptors() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation (mirrors the other statusq tests).
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1.
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Payload: packed virtio_input_event entries. Split across two descriptors at a non-8-byte
    // boundary to ensure the device model handles partial events across descriptor boundaries.
    let payload = [
        input_event_bytes(EV_LED, LED_NUML, 1),
        input_event_bytes(EV_LED, LED_CAPSL, 1),
        input_event_bytes(EV_LED, LED_SCROLLL, 0),
        input_event_bytes(EV_LED, LED_COMPOSE, 1),
        input_event_bytes(EV_LED, LED_KANA, 1),
        input_event_bytes(EV_SYN, SYN_REPORT, 0),
    ]
    .concat();

    let split = 11usize;
    let buf0 = 0x8000;
    let buf1 = 0x9000;
    mem.write(buf0, &payload[..split]).unwrap();
    mem.write(buf1, &payload[split..]).unwrap();

    write_desc(&mut mem, desc, 0, buf0, split as u32, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut mem,
        desc,
        1,
        buf1,
        (payload.len() - split) as u32,
        0,
        0,
    );

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    // The chain should be completed.
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);

    // And the LED state should be updated (bit0=num, bit1=caps, bit2=scroll, bit3=compose, bit4=kana).
    let leds = dev.device_mut::<VirtioInput>().unwrap().leds_mask();
    assert_eq!(leds, 0b11011);
}

#[test]
fn virtio_input_statusq_led_events_without_syn_report_still_apply() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation (mirrors the other statusq tests).
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1.
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Payload contains LED events but no terminating SYN_REPORT. The device model should still
    // apply the staged LED mask at the end of the chain.
    let buf = 0x8000;
    let payload = [
        input_event_bytes(EV_LED, LED_NUML, 1),
        input_event_bytes(EV_LED, LED_CAPSL, 1),
        input_event_bytes(EV_LED, LED_SCROLLL, 0),
        input_event_bytes(EV_LED, LED_COMPOSE, 1),
        input_event_bytes(EV_LED, LED_KANA, 1),
    ]
    .concat();
    mem.write(buf, &payload).unwrap();

    write_desc(&mut mem, desc, 0, buf, payload.len() as u32, 0, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);

    let leds = dev.device_mut::<VirtioInput>().unwrap().leds_mask();
    assert_eq!(leds, 0b11011);
}

#[test]
fn virtio_input_statusq_led_events_ignore_trailing_partial_event() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation (mirrors the other statusq tests).
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1.
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Payload contains LED events + a terminating SYN_REPORT, followed by a partial trailing event
    // (not a multiple of 8 bytes). The device model should ignore the incomplete trailing event
    // while still applying the complete LED updates.
    let mut payload = [
        input_event_bytes(EV_LED, LED_NUML, 1),
        input_event_bytes(EV_LED, LED_CAPSL, 1),
        input_event_bytes(EV_LED, LED_SCROLLL, 0),
        input_event_bytes(EV_LED, LED_COMPOSE, 1),
        input_event_bytes(EV_LED, LED_KANA, 1),
        input_event_bytes(EV_SYN, SYN_REPORT, 0),
    ]
    .concat();

    // Start of an "LED_NUML = 0" event, but truncated.
    payload.extend_from_slice(&input_event_bytes(EV_LED, LED_NUML, 0)[..5]);

    let buf = 0x8000;
    mem.write(buf, &payload).unwrap();
    write_desc(&mut mem, desc, 0, buf, payload.len() as u32, 0, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);

    let leds = dev.device_mut::<VirtioInput>().unwrap().leds_mask();
    assert_eq!(leds, 0b11011);
}

#[test]
fn virtio_input_statusq_invalid_descriptor_memory_is_ignored() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation (mirrors the other statusq tests).
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1 with two buffers. The first is valid and sets NumLock; the second
    // points outside guest memory and should be ignored without crashing or resetting state.
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 2);

    let buf0 = 0x8000;
    let payload = [
        input_event_bytes(EV_LED, LED_NUML, 1),
        input_event_bytes(EV_SYN, SYN_REPORT, 0),
    ]
    .concat();
    mem.write(buf0, &payload).unwrap();

    write_desc(&mut mem, desc, 0, buf0, payload.len() as u32, 0, 0);
    write_desc(
        &mut mem, desc, 1, 0x20000, // outside guest memory
        48, 0, 0,
    );

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 2).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, avail + 6, 1).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    // Both buffers should be consumed and completed.
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 2);
    let len0 = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len0, 0);
    let len1 = read_u32_le(&mem, used + 4 + 8 + 4).unwrap();
    assert_eq!(len1, 0);

    // And the LED state should reflect only the valid payload.
    let leds = dev.device_mut::<VirtioInput>().unwrap().leds_mask();
    assert_eq!(leds, 0b00001);
}

#[test]
fn virtio_input_statusq_led_parsing_is_bounded() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation (mirrors the other statusq tests).
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1 with a single oversized buffer. The device model should limit how
    // much it parses from statusq for safety. MAX_STATUSQ_BYTES is currently 4096, so place a LED
    // event exactly at that boundary and ensure it is ignored.
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    let buf = 0x8000;
    let mut payload = vec![0u8; 5000];
    payload[0..8].copy_from_slice(&input_event_bytes(EV_LED, LED_CAPSL, 1));
    payload[8..16].copy_from_slice(&input_event_bytes(EV_SYN, SYN_REPORT, 0));

    // Beyond the parsing budget.
    payload[4096..4104].copy_from_slice(&input_event_bytes(EV_LED, LED_NUML, 1));
    payload[4104..4112].copy_from_slice(&input_event_bytes(EV_SYN, SYN_REPORT, 0));

    mem.write(buf, &payload).unwrap();
    write_desc(&mut mem, desc, 0, buf, payload.len() as u32, 0, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);

    // Only CapsLock should be applied (bit1). NumLock event is beyond the parsing budget.
    let leds = dev.device_mut::<VirtioInput>().unwrap().leds_mask();
    assert_eq!(leds, 0b00010);
}

#[test]
fn virtio_input_statusq_mouse_ignores_led_events_but_consumes_buffers() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Mouse);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1.
    let desc = 0x5000;
    let avail = 0x6000;
    let used = 0x7000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 1);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Non-keyboard devices do not advertise EV_LED, but the device model still must consume and
    // complete statusq buffers. Feed it some LED events anyway and ensure they are ignored.
    let buf = 0x8000;
    let payload = [
        input_event_bytes(EV_LED, LED_NUML, 1),
        input_event_bytes(EV_LED, LED_CAPSL, 1),
        input_event_bytes(EV_SYN, SYN_REPORT, 0),
    ]
    .concat();
    mem.write(buf, &payload).unwrap();
    write_desc(&mut mem, desc, 0, buf, payload.len() as u32, 0, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let len = read_u32_le(&mem, used + 4 + 4).unwrap();
    assert_eq!(len, 0);

    // LEDs should remain at the default state for the mouse variant.
    let leds = dev.device_mut::<VirtioInput>().unwrap().leds_mask();
    assert_eq!(leds, 0);
}

#[test]
fn virtio_input_config_exposes_name_devids_and_ev_bits() {
    let keyboard = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(keyboard), Box::new(InterruptLog::default()));
    // Enable PCI memory decoding so BAR0 MMIO reads/writes reach the transport.
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x10000);

    // ID_NAME (NUL-terminated string).
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_ID_NAME);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    let payload = bar_read(&mut dev, caps.device + 8, size);
    assert!(payload.starts_with(b"Aero Virtio Keyboard"));
    assert_eq!(payload.last().copied(), Some(0));

    // ID_DEVIDS (BUS_VIRTUAL, virtio vendor id, keyboard product id, version).
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_ID_DEVIDS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);
    assert_eq!(bar_read_u8(&mut dev, caps.device + 2), 8);
    let payload = bar_read(&mut dev, caps.device + 8, 8);
    assert_eq!(
        payload,
        [
            0x06, 0x00, // bustype
            0xf4, 0x1a, // vendor
            0x01, 0x00, // product
            0x01, 0x00 // version
        ]
    );

    // EV_BITS: subsel=0 returns supported event types (keyboard: SYN/KEY/LED).
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    assert_eq!(size, 128);
    let ev_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(ev_bits[(EV_SYN / 8) as usize] & (1u8 << (EV_SYN % 8)), 0);
    assert_ne!(ev_bits[(EV_KEY / 8) as usize] & (1u8 << (EV_KEY % 8)), 0);
    assert_ne!(ev_bits[(EV_LED / 8) as usize] & (1u8 << (EV_LED % 8)), 0);
    assert_eq!(ev_bits[(EV_REL / 8) as usize] & (1u8 << (EV_REL % 8)), 0);
    assert_eq!(ev_bits[(EV_ABS / 8) as usize] & (1u8 << (EV_ABS % 8)), 0);

    // EV_BITS: subsel=EV_KEY returns supported key bitmap (keyboard should include KEY_A).
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, EV_KEY as u8);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    assert_eq!(size, 128);
    let key_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(key_bits[(KEY_A / 8) as usize] & (1u8 << (KEY_A % 8)), 0);
    // Contract-required keys (Win7 virtio-input): function keys + lock keys.
    assert_ne!(key_bits[(KEY_F1 / 8) as usize] & (1u8 << (KEY_F1 % 8)), 0);
    assert_ne!(key_bits[(KEY_F12 / 8) as usize] & (1u8 << (KEY_F12 % 8)), 0);
    assert_ne!(
        key_bits[(KEY_NUMLOCK / 8) as usize] & (1u8 << (KEY_NUMLOCK % 8)),
        0
    );
    assert_ne!(
        key_bits[(KEY_SCROLLLOCK / 8) as usize] & (1u8 << (KEY_SCROLLLOCK % 8)),
        0
    );
    // Consumer/media keys (used by the Win7 virtio-input driver Consumer Control collection).
    assert_ne!(
        key_bits[(KEY_MUTE / 8) as usize] & (1u8 << (KEY_MUTE % 8)),
        0
    );
    assert_ne!(
        key_bits[(KEY_VOLUMEDOWN / 8) as usize] & (1u8 << (KEY_VOLUMEDOWN % 8)),
        0
    );
    assert_ne!(
        key_bits[(KEY_VOLUMEUP / 8) as usize] & (1u8 << (KEY_VOLUMEUP % 8)),
        0
    );
    assert_ne!(
        key_bits[(KEY_NEXTSONG / 8) as usize] & (1u8 << (KEY_NEXTSONG % 8)),
        0
    );
    assert_ne!(
        key_bits[(KEY_PLAYPAUSE / 8) as usize] & (1u8 << (KEY_PLAYPAUSE % 8)),
        0
    );
    assert_ne!(
        key_bits[(KEY_PREVIOUSSONG / 8) as usize] & (1u8 << (KEY_PREVIOUSSONG % 8)),
        0
    );
    assert_ne!(
        key_bits[(KEY_STOPCD / 8) as usize] & (1u8 << (KEY_STOPCD % 8)),
        0
    );

    // EV_BITS: subsel=EV_LED returns supported LED bitmap (keyboard should include common LEDs).
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, EV_LED as u8);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    assert_eq!(size, 128);
    let led_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(
        led_bits[(LED_NUML / 8) as usize] & (1u8 << (LED_NUML % 8)),
        0
    );
    assert_ne!(
        led_bits[(LED_CAPSL / 8) as usize] & (1u8 << (LED_CAPSL % 8)),
        0
    );
    assert_ne!(
        led_bits[(LED_SCROLLL / 8) as usize] & (1u8 << (LED_SCROLLL % 8)),
        0
    );
    assert_ne!(
        led_bits[(LED_COMPOSE / 8) as usize] & (1u8 << (LED_COMPOSE % 8)),
        0
    );
    assert_ne!(
        led_bits[(LED_KANA / 8) as usize] & (1u8 << (LED_KANA % 8)),
        0
    );

    // Mouse variant exposes a different name and capability bitmap.
    let mouse = VirtioInput::new(VirtioInputDeviceKind::Mouse);
    let mut dev = VirtioPciDevice::new(Box::new(mouse), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);

    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_ID_NAME);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    let payload = bar_read(&mut dev, caps.device + 8, size);
    assert!(payload.starts_with(b"Aero Virtio Mouse"));

    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    let ev_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(ev_bits[(EV_SYN / 8) as usize] & (1u8 << (EV_SYN % 8)), 0);
    assert_ne!(ev_bits[(EV_KEY / 8) as usize] & (1u8 << (EV_KEY % 8)), 0);
    assert_ne!(ev_bits[(EV_REL / 8) as usize] & (1u8 << (EV_REL % 8)), 0);
    assert_eq!(ev_bits[(EV_LED / 8) as usize] & (1u8 << (EV_LED % 8)), 0);
    assert_eq!(ev_bits[(EV_ABS / 8) as usize] & (1u8 << (EV_ABS % 8)), 0);

    // Mouse key bitmap includes BTN_LEFT.
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, EV_KEY as u8);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    let key_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(
        key_bits[(BTN_LEFT / 8) as usize] & (1u8 << (BTN_LEFT % 8)),
        0
    );
    assert_ne!(
        key_bits[(BTN_RIGHT / 8) as usize] & (1u8 << (BTN_RIGHT % 8)),
        0
    );
    assert_ne!(
        key_bits[(BTN_MIDDLE / 8) as usize] & (1u8 << (BTN_MIDDLE % 8)),
        0
    );
    assert_ne!(
        key_bits[(BTN_SIDE / 8) as usize] & (1u8 << (BTN_SIDE % 8)),
        0
    );
    assert_ne!(
        key_bits[(BTN_EXTRA / 8) as usize] & (1u8 << (BTN_EXTRA % 8)),
        0
    );
    assert_ne!(
        key_bits[(BTN_FORWARD / 8) as usize] & (1u8 << (BTN_FORWARD % 8)),
        0
    );
    assert_ne!(
        key_bits[(BTN_BACK / 8) as usize] & (1u8 << (BTN_BACK % 8)),
        0
    );
    assert_ne!(
        key_bits[(BTN_TASK / 8) as usize] & (1u8 << (BTN_TASK % 8)),
        0
    );

    // Mouse rel bitmap includes REL_X/REL_Y/REL_WHEEL/REL_HWHEEL.
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, EV_REL as u8);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    let rel_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(rel_bits[(REL_X / 8) as usize] & (1u8 << (REL_X % 8)), 0);
    assert_ne!(rel_bits[(REL_Y / 8) as usize] & (1u8 << (REL_Y % 8)), 0);
    assert_ne!(
        rel_bits[(REL_WHEEL / 8) as usize] & (1u8 << (REL_WHEEL % 8)),
        0
    );
    assert_ne!(
        rel_bits[(REL_HWHEEL / 8) as usize] & (1u8 << (REL_HWHEEL % 8)),
        0
    );

    // Tablet variant (absolute pointer) exposes EV_ABS and ABS_INFO.
    let tablet = VirtioInput::new(VirtioInputDeviceKind::Tablet);
    let mut dev = VirtioPciDevice::new(Box::new(tablet), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);

    // ID_NAME: "Aero Virtio Tablet"
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_ID_NAME);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    let payload = bar_read(&mut dev, caps.device + 8, size);
    assert!(payload.starts_with(b"Aero Virtio Tablet"));

    // EV_BITS(types) should include EV_ABS but not EV_REL/EV_LED.
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    let ev_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(ev_bits[(EV_SYN / 8) as usize] & (1u8 << (EV_SYN % 8)), 0);
    assert_ne!(ev_bits[(EV_KEY / 8) as usize] & (1u8 << (EV_KEY % 8)), 0);
    assert_ne!(ev_bits[(EV_ABS / 8) as usize] & (1u8 << (EV_ABS % 8)), 0);
    assert_eq!(ev_bits[(EV_REL / 8) as usize] & (1u8 << (EV_REL % 8)), 0);
    assert_eq!(ev_bits[(EV_LED / 8) as usize] & (1u8 << (EV_LED % 8)), 0);

    // EV_BITS(EV_ABS) should include ABS_X/ABS_Y.
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_EV_BITS);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, EV_ABS as u8);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    assert_eq!(size, 128);
    let abs_bits = bar_read(&mut dev, caps.device + 8, size);
    assert_ne!(abs_bits[(ABS_X / 8) as usize] & (1u8 << (ABS_X % 8)), 0);
    assert_ne!(abs_bits[(ABS_Y / 8) as usize] & (1u8 << (ABS_Y % 8)), 0);

    // ABS_INFO(ABS_X) should return a deterministic range (0..32767).
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_ABS_INFO);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, ABS_X as u8);
    let size = bar_read_u8(&mut dev, caps.device + 2) as usize;
    assert_eq!(size, 20);
    let abs_info = bar_read(&mut dev, caps.device + 8, size);
    let min = i32::from_le_bytes(abs_info[0..4].try_into().unwrap());
    let max = i32::from_le_bytes(abs_info[4..8].try_into().unwrap());
    assert_eq!(min, 0);
    assert_eq!(max, 32767);
}

#[test]
fn virtio_input_malformed_descriptor_chain_does_not_wedge_queue() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));

    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x10000);

    // Configure event queue 0.
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 0);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Descriptor 0 loops back to itself (NEXT=1, next=0). The queue parser should treat this as a
    // malformed chain, but still advance used->idx so the guest doesn't wedge waiting forever.
    write_desc(&mut mem, desc, 0, 0x4000, 8, VIRTQ_DESC_F_NEXT, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    let used_id = read_u32_le(&mem, used + 4).unwrap();
    let used_len = read_u32_le(&mem, used + 8).unwrap();
    assert_eq!(used_id, 0);
    assert_eq!(used_len, 0);
}

#[test]
fn virtio_pci_common_cfg_out_of_range_queue_select_reads_zero_and_ignores_writes() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    // Virtio-input exposes 2 queues (eventq + statusq). Select a non-existent queue index.
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 7);

    // Contract v1 requires queue_size and queue_notify_off to read as 0 for out-of-range indices.
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x18), 0);
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x1e), 0);

    // Writes to queue registers must be ignored and must not silently affect queue 0.
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, 0xdead_beef);
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x16), 7);

    // Selecting queue 0 should still show the default (unconfigured) addresses.
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 0);
    let desc_lo = bar_read_u32(&mut dev, caps.common + 0x20);
    let desc_hi = bar_read_u32(&mut dev, caps.common + 0x24);
    assert_eq!((u64::from(desc_hi) << 32) | u64::from(desc_lo), 0);
}

#[test]
fn virtio_pci_queue_notify_off_matches_queue_index() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    for q in 0u16..2 {
        bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, q);
        assert_eq!(bar_read_u16(&mut dev, caps.common + 0x1e), q);
    }
}

#[test]
fn virtio_pci_reserved_feature_select_reads_zero_and_ignores_writes() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    // device_feature_select values other than 0 or 1 must read as 0.
    bar_write_u32(&mut dev, &mut mem, caps.common, 2);
    assert_eq!(bar_read_u32(&mut dev, caps.common + 0x04), 0);

    // driver_feature_select values other than 0 or 1 must read as 0 and ignore writes.
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 2);
    assert_eq!(bar_read_u32(&mut dev, caps.common + 0x0c), 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, 0xffff_ffff);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    assert_eq!(bar_read_u32(&mut dev, caps.common + 0x0c), 0);
}

#[test]
fn virtio_pci_queue_size_is_read_only() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 0);
    let original_size = bar_read_u16(&mut dev, caps.common + 0x18);
    assert_eq!(original_size, 64);

    // Contract v1 fixes queue sizes; writes must be ignored.
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x18, 8);
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x18), original_size);
}

#[test]
fn virtio_pci_notify_accepts_32bit_writes() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));

    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
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

    // Configure status queue 1.
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

    // Contract v1 requires notify to accept 32-bit writes too.
    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u32.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
}

#[test]
fn virtio_pci_clears_features_ok_when_driver_sets_unsupported_bits() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    // Basic status transition.
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // Read offered features.
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);

    // Write the offered features, plus one unsupported bit (EVENT_IDX = bit 29).
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0 | (1u32 << 29));
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f1);

    // Setting FEATURES_OK should trigger negotiation and the device should clear the bit.
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    let status = bar_read_u8(&mut dev, caps.common + 0x14);
    assert_eq!(status & VIRTIO_STATUS_FEATURES_OK, 0);
}

#[test]
fn virtio_pci_clears_features_ok_when_driver_omits_version_1_in_modern_mode() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // Read offered features.
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);

    // Negotiate all the offered features except VERSION_1. The device must reject
    // this in modern mode (contract v1 requires VERSION_1).
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 1);
    let version_1_hi = (VIRTIO_F_VERSION_1 >> 32) as u32;
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f1 & !version_1_hi);

    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    let status = bar_read_u8(&mut dev, caps.common + 0x14);
    assert_eq!(status & VIRTIO_STATUS_FEATURES_OK, 0);
}

#[derive(Default)]
struct LegacyIrqState {
    raised: u32,
    lowered: u32,
    asserted: bool,
}

#[derive(Clone)]
struct SharedLegacyIrq {
    state: Rc<RefCell<LegacyIrqState>>,
}

impl SharedLegacyIrq {
    fn new() -> (Self, Rc<RefCell<LegacyIrqState>>) {
        let state = Rc::new(RefCell::new(LegacyIrqState::default()));
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

impl InterruptSink for SharedLegacyIrq {
    fn raise_legacy_irq(&mut self) {
        let mut state = self.state.borrow_mut();
        state.raised = state.raised.saturating_add(1);
        state.asserted = true;
    }

    fn lower_legacy_irq(&mut self) {
        let mut state = self.state.borrow_mut();
        state.lowered = state.lowered.saturating_add(1);
        state.asserted = false;
    }

    fn signal_msix(&mut self, _message: MsiMessage) {}
}

#[test]
fn virtio_pci_reset_deasserts_intx_and_clears_isr() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let (irq, irq_state) = SharedLegacyIrq::new();
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(irq));

    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    // Standard init and feature negotiation.
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
    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f1);
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
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

    // Post one event buffer.
    let event_buf = 0x4000;
    mem.write(event_buf, &[0u8; 8]).unwrap();
    write_desc(&mut mem, desc, 0, event_buf, 8, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    // Queue kick only makes the buffer available; it should not raise an interrupt.
    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);
    {
        let state = irq_state.borrow();
        assert_eq!(state.raised, 0);
        assert!(!state.asserted);
    }

    // Host injects an input event and the device should raise an interrupt.
    dev.device_mut::<VirtioInput>()
        .unwrap()
        .push_event(VirtioInputEvent {
            type_: 1,
            code: 30,
            value: 1,
        });
    dev.poll(&mut mem);
    {
        let state = irq_state.borrow();
        assert_eq!(state.raised, 1);
        assert_eq!(state.lowered, 0);
        assert!(state.asserted);
    }

    // Reset must clear ISR state and deassert INTx even if the guest never read ISR.
    bar_write_u8(&mut dev, &mut mem, caps.common + 0x14, 0);
    {
        let state = irq_state.borrow();
        assert_eq!(state.raised, 1);
        assert_eq!(state.lowered, 1);
        assert!(!state.asserted);
    }
    assert_eq!(bar_read_u8(&mut dev, caps.isr), 0);
}

#[test]
fn virtio_pci_device_cfg_writes_do_not_raise_config_interrupt() {
    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let (irq, irq_state) = SharedLegacyIrq::new();
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(irq));
    dev.config_write(0x04, &0x0002u16.to_le_bytes());
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x10000);

    // Device config writes are used by virtio-input selector probing. They must not
    // trigger CONFIG interrupts; config IRQs are reserved for device-side changes.
    bar_write_u8(&mut dev, &mut mem, caps.device, VIRTIO_INPUT_CFG_ID_NAME);
    bar_write_u8(&mut dev, &mut mem, caps.device + 1, 0);

    let state = irq_state.borrow();
    assert_eq!(state.raised, 0);
    assert_eq!(state.lowered, 0);
    assert!(!state.asserted);

    drop(state);
    assert_eq!(bar_read_u8(&mut dev, caps.isr), 0);
}
