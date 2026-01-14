#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use aero_virtio::devices::input::{EV_KEY, EV_LED, EV_SYN, KEY_A, LED_CAPSL, SYN_REPORT};
use aero_virtio::pci::{
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_WRITE;
use pretty_assertions::assert_eq;

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_write(0xCFC + (offset & 3), size, value);
}

fn bar0_base(m: &mut Machine, bdf: PciBdf) -> u64 {
    let bar0_lo = cfg_read(m, bdf, 0x10, 4);
    let bar0_hi = cfg_read(m, bdf, 0x14, 4);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xFFFF_FFF0)
}

fn write_desc(m: &mut Machine, table: u64, index: u16, addr: u64, len: u32, flags: u16) {
    let base = table + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, 0);
}

fn machine_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

#[test]
fn snapshot_roundtrip_preserves_virtio_input_queue_state_without_dup_or_stuck_irq() {
    let mut m = Machine::new(machine_cfg()).unwrap();

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0);

    // Enable PCI BAR0 decoding + bus mastering.
    let mut cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cmd |= 0x0006; // MEM + BUSMASTER
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd));

    let common = bar0;
    let notify = bar0 + 0x1000;

    // Feature negotiation (modern virtio-pci).
    m.write_physical_u8(common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    m.write_physical_u32(common, 0);
    let f0 = m.read_physical_u32(common + 0x04);
    m.write_physical_u32(common + 0x08, 0);
    m.write_physical_u32(common + 0x0c, f0);

    m.write_physical_u32(common, 1);
    let f1 = m.read_physical_u32(common + 0x04);
    m.write_physical_u32(common + 0x08, 1);
    m.write_physical_u32(common + 0x0c, f1);

    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    assert_ne!(
        m.read_physical_u8(common + 0x14) & VIRTIO_STATUS_FEATURES_OK,
        0
    );
    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure event queue 0.
    let desc = 0x10000u64;
    let avail = 0x11000u64;
    let used = 0x12000u64;

    m.write_physical_u16(common + 0x16, 0); // queue_select
    m.write_physical_u64(common + 0x20, desc);
    m.write_physical_u64(common + 0x28, avail);
    m.write_physical_u64(common + 0x30, used);
    m.write_physical_u16(common + 0x1c, 1); // queue_enable

    // Post 2 buffers; do not inject any events yet.
    let bufs = [0x13000u64, 0x13020u64];
    for (i, &buf) in bufs.iter().enumerate() {
        m.write_physical(buf, &[0u8; 8]);
        write_desc(&mut m, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
    }
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _buf) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0 and let the device consume the buffers (it will cache them internally).
    m.write_physical_u16(notify, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 0);

    // No pending interrupts expected yet.
    assert!(!m.virtio_input_keyboard().unwrap().borrow().irq_level());

    let snap = m.take_snapshot_full().unwrap();

    let mut restored = Machine::new(machine_cfg()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Device status should survive (DRIVER_OK).
    assert!(restored.virtio_input_keyboard_driver_ok());

    // Snapshot restore should not spuriously assert INTx without an event.
    assert!(!restored
        .virtio_input_keyboard()
        .unwrap()
        .borrow()
        .irq_level());

    // The cached-but-not-used buffers must still be usable after restore: inject a key press and
    // expect both EV_KEY and EV_SYN to complete immediately without reusing/duplicating old events.
    restored.inject_virtio_key(KEY_A, true);

    assert_eq!(restored.read_physical_u16(used + 2), 2);
    let got0 = restored.read_physical_bytes(bufs[0], 8);
    let got1 = restored.read_physical_bytes(bufs[1], 8);

    let parse = |bytes: &[u8]| {
        let type_ = u16::from_le_bytes([bytes[0], bytes[1]]);
        let code = u16::from_le_bytes([bytes[2], bytes[3]]);
        let value = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        (type_, code, value)
    };

    assert_eq!(parse(&got0), (EV_KEY, KEY_A, 1));
    assert_eq!(parse(&got1), (EV_SYN, SYN_REPORT, 0));
}

#[test]
fn snapshot_roundtrip_preserves_virtio_input_keyboard_leds_mask() {
    let mut m = Machine::new(machine_cfg()).unwrap();

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0);

    // Enable PCI BAR0 decoding + bus mastering.
    let mut cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cmd |= 0x0006; // MEM + BUSMASTER
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd));

    let common = bar0;
    let notify = bar0 + 0x1000;
    let isr = bar0 + u64::from(profile::VIRTIO_ISR_CFG_BAR0_OFFSET);

    // Feature negotiation (modern virtio-pci).
    m.write_physical_u8(common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    m.write_physical_u32(common, 0);
    let f0 = m.read_physical_u32(common + 0x04);
    m.write_physical_u32(common + 0x08, 0);
    m.write_physical_u32(common + 0x0c, f0);

    m.write_physical_u32(common, 1);
    let f1 = m.read_physical_u32(common + 0x04);
    m.write_physical_u32(common + 0x08, 1);
    m.write_physical_u32(common + 0x0c, f1);

    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    assert_ne!(
        m.read_physical_u8(common + 0x14) & VIRTIO_STATUS_FEATURES_OK,
        0
    );
    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );
    assert!(m.virtio_input_keyboard_driver_ok());

    // Configure status queue 1.
    let desc = 0x20000u64;
    let avail = 0x21000u64;
    let used = 0x22000u64;

    m.write_physical_u16(common + 0x16, 1); // queue_select
    m.write_physical_u64(common + 0x20, desc);
    m.write_physical_u64(common + 0x28, avail);
    m.write_physical_u64(common + 0x30, used);
    m.write_physical_u16(common + 0x1c, 1); // queue_enable

    // statusq payload: [EV_LED, LED_CAPSL, 1] + [EV_SYN, SYN_REPORT, 0]
    let buf = 0x23000u64;
    let mut payload = [0u8; 16];
    payload[0..2].copy_from_slice(&EV_LED.to_le_bytes());
    payload[2..4].copy_from_slice(&LED_CAPSL.to_le_bytes());
    payload[4..8].copy_from_slice(&1i32.to_le_bytes());
    payload[8..10].copy_from_slice(&EV_SYN.to_le_bytes());
    payload[10..12].copy_from_slice(&SYN_REPORT.to_le_bytes());
    payload[12..16].copy_from_slice(&0i32.to_le_bytes());
    m.write_physical(buf, &payload);
    write_desc(&mut m, desc, 0, buf, payload.len() as u32, 0);

    // avail ring: flags=0, idx=1, ring[0]=0.
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 1);
    m.write_physical_u16(avail + 4, 0);

    // used ring: flags=0, idx=0.
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 1.
    let notify_off = m.read_physical_u16(common + 0x1e);
    let notify_addr =
        notify + u64::from(notify_off) * u64::from(profile::VIRTIO_NOTIFY_OFF_MULTIPLIER);
    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 1);
    assert_eq!(m.virtio_input_keyboard_leds(), 0x02);

    // Clear any pending IRQ before snapshot so restore can be checked for spurious interrupts.
    let _ = m.read_physical_u8(isr);
    assert!(!m.virtio_input_keyboard().unwrap().borrow().irq_level());

    let snap = m.take_snapshot_full().unwrap();

    let mut restored = Machine::new(machine_cfg()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    assert!(restored.virtio_input_keyboard_driver_ok());
    assert_eq!(restored.virtio_input_keyboard_leds(), 0x02);
    assert!(
        !restored.virtio_input_keyboard().unwrap().borrow().irq_level(),
        "restore should not spuriously assert IRQ for virtio-input keyboard"
    );
}
