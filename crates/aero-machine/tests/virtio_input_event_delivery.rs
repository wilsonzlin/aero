#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use aero_virtio::devices::input::{
    VirtioInput, BTN_LEFT, EV_KEY, EV_LED, EV_REL, EV_SYN, KEY_A, KEY_VOLUMEUP, LED_CAPSL,
    REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, SYN_REPORT,
};
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

#[test]
fn inject_input_batch_mouse_buttons_after_snapshot_restore_still_delivers_press_to_virtio() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    let bdf = profile::VIRTIO_INPUT_MOUSE.bdf;
    let bar0 = bar0_base(&mut src, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");

    // Enable PCI BAR0 MMIO decoding + bus mastering (virtio DMA).
    let mut cmd = cfg_read(&mut src, bdf, 0x04, 2) as u16;
    cmd |= 0x0006; // MEM + BUSMASTER
    cfg_write(&mut src, bdf, 0x04, 2, u32::from(cmd));

    let common = bar0;
    let notify = bar0 + 0x1000;

    // Feature negotiation (modern virtio-pci).
    src.write_physical_u8(common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    src.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // Read offered features (low/high dwords) and accept them as-is.
    src.write_physical_u32(common, 0);
    let f0 = src.read_physical_u32(common + 0x04);
    src.write_physical_u32(common + 0x08, 0);
    src.write_physical_u32(common + 0x0c, f0);

    src.write_physical_u32(common, 1);
    let f1 = src.read_physical_u32(common + 0x04);
    src.write_physical_u32(common + 0x08, 1);
    src.write_physical_u32(common + 0x0c, f1);

    src.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    assert_ne!(
        src.read_physical_u8(common + 0x14) & VIRTIO_STATUS_FEATURES_OK,
        0
    );
    src.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );
    assert!(src.virtio_input_mouse_driver_ok());

    // Configure virtqueue 0 (eventq).
    let desc = 0x10000u64;
    let avail = 0x11000u64;
    let used = 0x12000u64;

    src.write_physical_u16(common + 0x16, 0); // queue_select
    src.write_physical_u64(common + 0x20, desc);
    src.write_physical_u64(common + 0x28, avail);
    src.write_physical_u64(common + 0x30, used);
    src.write_physical_u16(common + 0x1c, 1); // queue_enable

    // Post enough event buffers to cover the worst-case "unknown previous state" resync behavior.
    let mut bufs = Vec::new();
    for i in 0..32u64 {
        let buf = 0x13000u64 + i * 0x20;
        src.write_physical(buf, &[0u8; 8]);
        write_desc(&mut src, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
        bufs.push(buf);
    }

    // avail ring: flags=0, idx=bufs.len(), ring=[0..N-1].
    src.write_physical_u16(avail, 0);
    src.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _buf) in bufs.iter().enumerate() {
        src.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }

    // used ring: flags=0, idx=0.
    src.write_physical_u16(used, 0);
    src.write_physical_u16(used + 2, 0);

    // Notify queue 0 (virtio-pci modern notify region) so the device caches the buffers.
    src.write_physical_u16(notify, 0);
    src.process_virtio_input();
    assert_eq!(src.read_physical_u16(used + 2), 0);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
    assert!(
        restored.virtio_input_mouse_driver_ok(),
        "virtio-input mouse should remain DRIVER_OK after restore"
    );

    // MouseButtons: press left button (DOM bit0). This should still deliver a BTN_LEFT press event
    // to virtio-input even though snapshot restore invalidates host-side previous-button caches.
    let words: [u32; 6] = [1, 0, 3, 0, 0x01, 0];
    restored.inject_input_batch(&words);

    let used_idx = restored.read_physical_u16(used + 2) as usize;
    assert!(used_idx > 0, "expected at least one virtio input event");

    let mut saw_left_down = false;
    for i in 0..used_idx.min(bufs.len()) {
        let got = restored.read_physical_bytes(bufs[i], 8);
        let type_ = u16::from_le_bytes([got[0], got[1]]);
        let code_ = u16::from_le_bytes([got[2], got[3]]);
        let value_ = i32::from_le_bytes([got[4], got[5], got[6], got[7]]);
        if (type_, code_, value_) == (EV_KEY, BTN_LEFT, 1) {
            saw_left_down = true;
            break;
        }
    }
    assert!(
        saw_left_down,
        "expected virtio-input mouse to receive BTN_LEFT=1 after snapshot restore"
    );
}

#[test]
fn inject_input_batch_routes_consumer_usage_to_virtio_keyboard_when_driver_ok() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");

    // Enable PCI BAR0 MMIO decoding + bus mastering (virtio DMA).
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

    // Read offered features (low/high dwords) and accept them as-is.
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
        0,
        "device should accept FEATURES_OK"
    );

    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );
    assert!(m.virtio_input_keyboard_driver_ok());

    // Configure virtqueue 0 (eventq).
    let desc = 0x10000u64;
    let avail = 0x11000u64;
    let used = 0x12000u64;

    m.write_physical_u16(common + 0x16, 0); // queue_select
    m.write_physical_u64(common + 0x20, desc);
    m.write_physical_u64(common + 0x28, avail);
    m.write_physical_u64(common + 0x30, used);
    m.write_physical_u16(common + 0x1c, 1); // queue_enable

    // Post 2 event buffers so EV_KEY + EV_SYN can be delivered immediately.
    let bufs = [0x13000u64, 0x13020u64];
    for (i, &buf) in bufs.iter().enumerate() {
        m.write_physical(buf, &[0u8; 8]);
        write_desc(&mut m, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
    }

    // avail ring: flags=0, idx=2, ring=[0,1].
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _buf) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }

    // used ring: flags=0, idx=0.
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0 (virtio-pci modern notify region) and allow the device to cache the buffers.
    m.write_physical_u16(notify, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 0);

    // InputEventQueue wire format:
    //   [count, batch_ts, type, event_ts, a, b, ...]
    //
    // Inject a Consumer Control (Usage Page 0x0C) Volume Up press (usage_id=0x00e9).
    // The machine should route this through virtio-input when the guest driver is DRIVER_OK.
    let words: [u32; 6] = [
        1,
        0,
        7, // InputEventType.HidUsage16
        0,
        0x0001_000c, // (usage_page=0x000c) | ((pressed ? 1 : 0) << 16)
        0x00e9,      // usage_id
    ];
    m.inject_input_batch(&words);

    assert_eq!(m.read_physical_u16(used + 2), 2, "expected 2 used entries");

    let expected: [(u16, u16, i32); 2] = [(EV_KEY, KEY_VOLUMEUP, 1), (EV_SYN, SYN_REPORT, 0)];
    for (&buf, (ty, code, val)) in bufs.iter().zip(expected) {
        let got = m.read_physical_bytes(buf, 8);
        let type_ = u16::from_le_bytes([got[0], got[1]]);
        let code_ = u16::from_le_bytes([got[2], got[3]]);
        let value_ = i32::from_le_bytes([got[4], got[5], got[6], got[7]]);
        assert_eq!((type_, code_, value_), (ty, code, val));
    }
}

#[test]
fn virtio_input_keyboard_init_then_host_injects_key_events() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");

    // Enable PCI BAR0 MMIO decoding + bus mastering (virtio DMA).
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

    // Read offered features (low/high dwords) and accept them as-is.
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
    // Device should accept FEATURES_OK (must keep it set if negotiation succeeded).
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

    // Configure virtqueue 0 (eventq).
    let desc = 0x10000u64;
    let avail = 0x11000u64;
    let used = 0x12000u64;

    m.write_physical_u16(common + 0x16, 0); // queue_select
    m.write_physical_u64(common + 0x20, desc);
    m.write_physical_u64(common + 0x28, avail);
    m.write_physical_u64(common + 0x30, used);
    m.write_physical_u16(common + 0x1c, 1); // queue_enable

    // Post 4 event buffers so a key press+release (EV_KEY+EV_SYN x2) can be delivered immediately.
    let bufs = [0x13000u64, 0x13020u64, 0x13040u64, 0x13060u64];
    for (i, &buf) in bufs.iter().enumerate() {
        m.write_physical(buf, &[0u8; 8]);
        write_desc(&mut m, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
    }

    // avail ring: flags=0, idx=4, ring=[0,1,2,3].
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _buf) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }

    // used ring: flags=0, idx=0.
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0 (virtio-pci modern notify region).
    m.write_physical_u16(notify, 0);

    // Host injects key press + release.
    m.inject_virtio_key(KEY_A, true);
    m.inject_virtio_key(KEY_A, false);

    assert_eq!(m.read_physical_u16(used + 2), 4, "expected 4 used entries");

    // Verify the event payloads match `struct virtio_input_event` (little-endian).
    let expected: [(u16, u16, i32); 4] = [
        // press
        (EV_KEY, KEY_A, 1),
        (EV_SYN, SYN_REPORT, 0),
        // release
        (EV_KEY, KEY_A, 0),
        (EV_SYN, SYN_REPORT, 0),
    ];
    for (&buf, (ty, code, val)) in bufs.iter().zip(expected) {
        let got = m.read_physical_bytes(buf, 8);
        let type_ = u16::from_le_bytes([got[0], got[1]]);
        let code_ = u16::from_le_bytes([got[2], got[3]]);
        let value_ = i32::from_le_bytes([got[4], got[5], got[6], got[7]]);
        assert_eq!((type_, code_, value_), (ty, code, val));
    }
}

#[test]
fn virtio_input_keyboard_input_batch_routes_consumer_media_keys_via_virtio() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");

    // Enable PCI BAR0 MMIO decoding + bus mastering (virtio DMA).
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

    // Read offered features (low/high dwords) and accept them as-is.
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

    // Configure virtqueue 0 (eventq).
    let desc = 0x10000u64;
    let avail = 0x11000u64;
    let used = 0x12000u64;

    m.write_physical_u16(common + 0x16, 0); // queue_select
    m.write_physical_u64(common + 0x20, desc);
    m.write_physical_u64(common + 0x28, avail);
    m.write_physical_u64(common + 0x30, used);
    m.write_physical_u16(common + 0x1c, 1); // queue_enable

    // Post 4 event buffers so a key press+release (EV_KEY+EV_SYN x2) can be delivered immediately.
    let bufs = [0x13000u64, 0x13020u64, 0x13040u64, 0x13060u64];
    for (i, &buf) in bufs.iter().enumerate() {
        m.write_physical(buf, &[0u8; 8]);
        write_desc(&mut m, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
    }

    // avail ring: flags=0, idx=4, ring=[0,1,2,3].
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _buf) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }

    // used ring: flags=0, idx=0.
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0 (virtio-pci modern notify region).
    m.write_physical_u16(notify, 0);

    // InputEventQueue batch: Consumer Control "Volume Up" press+release (Usage Page 0x0C, Usage ID 0x00E9).
    //
    // This is delivered through virtio-input when DRIVER_OK is set (instead of requiring a synthetic USB
    // consumer-control device to be present/configured).
    let words: [u32; 10] = [
        2,
        0,
        // HidUsage16: ConsumerControl VolumeUp (press)
        7,
        0,
        0x0001_000c,
        0x00e9,
        // HidUsage16: ConsumerControl VolumeUp (release)
        7,
        0,
        0x0000_000c,
        0x00e9,
    ];
    m.inject_input_batch(&words);

    assert_eq!(m.read_physical_u16(used + 2), 4, "expected 4 used entries");

    // Verify the event payloads match `struct virtio_input_event` (little-endian).
    let expected: [(u16, u16, i32); 4] = [
        // press
        (EV_KEY, KEY_VOLUMEUP, 1),
        (EV_SYN, SYN_REPORT, 0),
        // release
        (EV_KEY, KEY_VOLUMEUP, 0),
        (EV_SYN, SYN_REPORT, 0),
    ];
    for (&buf, (ty, code, val)) in bufs.iter().zip(expected) {
        let got = m.read_physical_bytes(buf, 8);
        let type_ = u16::from_le_bytes([got[0], got[1]]);
        let code_ = u16::from_le_bytes([got[2], got[3]]);
        let value_ = i32::from_le_bytes([got[4], got[5], got[6], got[7]]);
        assert_eq!((type_, code_, value_), (ty, code, val));
    }
}

#[test]
fn virtio_input_mouse_init_then_host_injects_rel_button_and_wheel2_events() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let bdf = profile::VIRTIO_INPUT_MOUSE.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");

    // Enable PCI BAR0 MMIO decoding + bus mastering (virtio DMA).
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

    // Read offered features (low/high dwords) and accept them as-is.
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

    // Configure virtqueue 0 (eventq).
    let desc = 0x10000u64;
    let avail = 0x11000u64;
    let used = 0x12000u64;

    m.write_physical_u16(common + 0x16, 0); // queue_select
    m.write_physical_u64(common + 0x20, desc);
    m.write_physical_u64(common + 0x28, avail);
    m.write_physical_u64(common + 0x30, used);
    m.write_physical_u16(common + 0x1c, 1); // queue_enable

    // Post 8 event buffers:
    // - inject_rel -> REL_X + REL_Y + SYN
    // - inject_button -> EV_KEY + SYN
    // - inject_wheel2 -> REL_WHEEL + REL_HWHEEL + SYN
    // Total = 8 events.
    let bufs = [
        0x13000u64, 0x13020u64, 0x13040u64, 0x13060u64, 0x13080u64, 0x130A0u64, 0x130C0u64,
        0x130E0u64,
    ];
    for (i, &buf) in bufs.iter().enumerate() {
        m.write_physical(buf, &[0u8; 8]);
        write_desc(&mut m, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
    }

    // avail ring: flags=0, idx=8, ring=[0..7].
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _buf) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }

    // used ring: flags=0, idx=0.
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0 (virtio-pci modern notify region).
    m.write_physical_u16(notify, 0);

    // Inject a small batch of typical mouse events.
    m.inject_virtio_rel(5, -3);
    m.inject_virtio_button(BTN_LEFT, true);
    m.inject_virtio_wheel2(1, 2);

    assert_eq!(m.read_physical_u16(used + 2), 8, "expected 8 used entries");

    let expected: [(u16, u16, i32); 8] = [
        (EV_REL, REL_X, 5),
        (EV_REL, REL_Y, -3),
        (EV_SYN, SYN_REPORT, 0),
        (EV_KEY, BTN_LEFT, 1),
        (EV_SYN, SYN_REPORT, 0),
        (EV_REL, REL_WHEEL, 1),
        (EV_REL, REL_HWHEEL, 2),
        (EV_SYN, SYN_REPORT, 0),
    ];

    for (&buf, (ty, code, val)) in bufs.iter().zip(expected) {
        let got = m.read_physical_bytes(buf, 8);
        let type_ = u16::from_le_bytes([got[0], got[1]]);
        let code_ = u16::from_le_bytes([got[2], got[3]]);
        let value_ = i32::from_le_bytes([got[4], got[5], got[6], got[7]]);
        assert_eq!((type_, code_, value_), (ty, code, val));
    }
}

#[test]
fn virtio_input_keyboard_statusq_updates_leds_mask() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let virtio_kb = m
        .virtio_input_keyboard()
        .expect("virtio-input keyboard enabled");
    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");

    // Enable PCI BAR0 MMIO decoding + bus mastering (virtio DMA).
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

    // Read offered features (low/high dwords) and accept them as-is.
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

    // Configure virtqueue 1 (statusq). The guest uses this to report output events such as LEDs.
    let desc = 0x20000u64;
    let avail = 0x21000u64;
    let used = 0x22000u64;

    m.write_physical_u16(common + 0x16, 1); // queue_select
    m.write_physical_u64(common + 0x20, desc);
    m.write_physical_u64(common + 0x28, avail);
    m.write_physical_u64(common + 0x30, used);
    m.write_physical_u16(common + 0x1c, 1); // queue_enable

    let buf0 = 0x23000u64;
    let buf1 = 0x23020u64;

    // statusq payload: [EV_LED, LED_CAPSL, 1] + [EV_SYN, SYN_REPORT, 0]
    let mut payload0 = [0u8; 16];
    payload0[0..2].copy_from_slice(&EV_LED.to_le_bytes());
    payload0[2..4].copy_from_slice(&LED_CAPSL.to_le_bytes());
    payload0[4..8].copy_from_slice(&1i32.to_le_bytes());
    payload0[8..10].copy_from_slice(&EV_SYN.to_le_bytes());
    payload0[10..12].copy_from_slice(&SYN_REPORT.to_le_bytes());
    payload0[12..16].copy_from_slice(&0i32.to_le_bytes());

    // statusq payload: clear caps lock.
    let mut payload1 = payload0;
    payload1[4..8].copy_from_slice(&0i32.to_le_bytes());

    m.write_physical(buf0, &payload0);
    m.write_physical(buf1, &payload1);
    write_desc(&mut m, desc, 0, buf0, payload0.len() as u32, 0);
    write_desc(&mut m, desc, 1, buf1, payload1.len() as u32, 0);

    // avail ring: flags=0, idx=1, ring[0]=0.
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 1);
    m.write_physical_u16(avail + 4, 0);

    // used ring: flags=0, idx=0.
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 1 and allow the device to consume the chain.
    let notify_off = m.read_physical_u16(common + 0x1e);
    let notify_addr =
        notify + u64::from(notify_off) * u64::from(profile::VIRTIO_NOTIFY_OFF_MULTIPLIER);
    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_input();

    assert_eq!(m.read_physical_u16(used + 2), 1, "expected 1 used entry");
    let leds = virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .leds_mask();
    assert_eq!(leds, 0x02, "expected Caps Lock LED bit to be set");
    assert_eq!(
        m.virtio_input_keyboard_leds(),
        leds,
        "Machine::virtio_input_keyboard_leds should reflect device state"
    );

    // Post a second statusq buffer to clear the Caps Lock LED.
    m.write_physical_u16(avail + 2, 2); // idx
    m.write_physical_u16(avail + 6, 1); // ring[1]=1
    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_input();

    assert_eq!(m.read_physical_u16(used + 2), 2, "expected 2 used entries");
    let leds = virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .leds_mask();
    assert_eq!(leds, 0x00, "expected Caps Lock LED bit to be cleared");
    assert_eq!(
        m.virtio_input_keyboard_leds(),
        leds,
        "Machine::virtio_input_keyboard_leds should reflect device state"
    );
}

#[test]
fn virtio_input_keyboard_leds_can_be_read_while_device_is_borrowed() {
    let m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let kbd = m
        .virtio_input_keyboard()
        .expect("virtio-input keyboard should be present when enabled");

    // Hold an immutable borrow to the virtio-pci device and ensure the LED getter remains usable
    // (it should only need a shared borrow of the underlying VirtioInput device).
    let _held = kbd.borrow();
    assert_eq!(
        m.virtio_input_keyboard_leds(),
        0,
        "expected virtio_input_keyboard_leds to succeed under an existing device borrow"
    );
}
