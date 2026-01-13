#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use aero_virtio::devices::input::{EV_KEY, EV_SYN, KEY_A, SYN_REPORT};
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

    let common = bar0 + 0x0000;
    let notify = bar0 + 0x1000;

    // Feature negotiation (modern virtio-pci).
    m.write_physical_u8(common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

    // Read offered features (low/high dwords) and accept them as-is.
    m.write_physical_u32(common + 0x00, 0);
    let f0 = m.read_physical_u32(common + 0x04);
    m.write_physical_u32(common + 0x08, 0);
    m.write_physical_u32(common + 0x0c, f0);

    m.write_physical_u32(common + 0x00, 1);
    let f1 = m.read_physical_u32(common + 0x04);
    m.write_physical_u32(common + 0x08, 1);
    m.write_physical_u32(common + 0x0c, f1);

    m.write_physical_u8(
        common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    // Device should accept FEATURES_OK (must keep it set if negotiation succeeded).
    assert_ne!(m.read_physical_u8(common + 0x14) & VIRTIO_STATUS_FEATURES_OK, 0);

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
        write_desc(
            &mut m,
            desc,
            i as u16,
            buf,
            8,
            VIRTQ_DESC_F_WRITE,
        );
    }

    // avail ring: flags=0, idx=4, ring=[0,1,2,3].
    m.write_physical_u16(avail + 0, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _buf) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }

    // used ring: flags=0, idx=0.
    m.write_physical_u16(used + 0, 0);
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
