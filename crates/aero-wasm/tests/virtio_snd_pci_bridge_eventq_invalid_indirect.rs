#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::snd::{
    JACK_ID_SPEAKER, VIRTIO_SND_EVT_JACK_CONNECTED, VIRTIO_SND_QUEUE_EVENT,
};
use aero_virtio::pci::{
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{
    MAX_INDIRECT_DESC_TABLE_ENTRIES, VIRTQ_DESC_F_INDIRECT, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
};
use aero_wasm::VirtioSndPciBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn write_desc(
    guest: &common::GuestRegion,
    table: u32,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = table + u32::from(index) * 16;
    guest.write_u64(base, addr);
    guest.write_u32(base + 8, len);
    guest.write_u16(base + 12, flags);
    guest.write_u16(base + 14, next);
}

fn expected_speaker_connected() -> [u8; 8] {
    let mut evt = [0u8; 8];
    evt[0..4].copy_from_slice(&VIRTIO_SND_EVT_JACK_CONNECTED.to_le_bytes());
    evt[4..8].copy_from_slice(&JACK_ID_SPEAKER.to_le_bytes());
    evt
}

fn init_snd_eventq(bridge: &mut VirtioSndPciBridge) -> u16 {
    const COMMON: u32 = 0x0000;

    // Minimal virtio feature negotiation (accept everything offered).
    bridge.mmio_write(COMMON + 0x14, 1, u32::from(VIRTIO_STATUS_ACKNOWLEDGE));
    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER),
    );

    bridge.mmio_write(COMMON, 4, 0); // device_feature_select
    let f0 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 0); // driver_feature_select
    bridge.mmio_write(COMMON + 0x0c, 4, f0); // driver_features

    bridge.mmio_write(COMMON, 4, 1);
    let f1 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 1);
    bridge.mmio_write(COMMON + 0x0c, 4, f1);

    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK),
    );
    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        ),
    );

    // Configure event queue 1 (virtio-snd).
    bridge.mmio_write(COMMON + 0x16, 2, u32::from(VIRTIO_SND_QUEUE_EVENT)); // queue_select
    let qsz = bridge.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz >= 2, "expected event queue size >= 2");

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;

    bridge.mmio_write(COMMON + 0x20, 4, desc_table);
    bridge.mmio_write(COMMON + 0x24, 4, 0);
    bridge.mmio_write(COMMON + 0x28, 4, avail);
    bridge.mmio_write(COMMON + 0x2c, 4, 0);
    bridge.mmio_write(COMMON + 0x30, 4, used);
    bridge.mmio_write(COMMON + 0x34, 4, 0);
    bridge.mmio_write(COMMON + 0x1c, 2, 1); // queue_enable

    qsz
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_eventq_retains_event_when_first_chain_indirect_has_next_flag() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    bridge.set_pci_command(0x0006);

    let _qsz = init_snd_eventq(&mut bridge);

    const COMMON: u32 = 0x0000;
    const NOTIFY: u32 = 0x1000;

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;
    let indirect = 0x4000u32;
    let buf = 0x4100u32;

    // Invalid indirect descriptor: INDIRECT + NEXT is forbidden.
    write_desc(
        &guest,
        desc_table,
        0,
        indirect as u64,
        16,
        VIRTQ_DESC_F_INDIRECT | VIRTQ_DESC_F_NEXT,
        0,
    );
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 1, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    guest.write_u16(avail, 0);
    guest.write_u16(avail + 2, 2);
    guest.write_u16(avail + 4, 0);
    guest.write_u16(avail + 6, 1);
    guest.write_u16(used, 0);
    guest.write_u16(used + 2, 0);

    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");

    let notify_off = bridge.mmio_read(COMMON + 0x1e, 2);
    bridge.mmio_write(
        NOTIFY + notify_off * 4,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

    assert_eq!(guest.read_u16(used + 2), 2);
    assert_eq!(guest.read_u32(used + 4), 0);
    assert_eq!(guest.read_u32(used + 8), 0);
    assert_eq!(guest.read_u32(used + 12), 1);
    assert_eq!(guest.read_u32(used + 16), 8);

    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_eventq_retains_event_when_first_chain_indirect_table_too_large() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    bridge.set_pci_command(0x0006);

    let _qsz = init_snd_eventq(&mut bridge);

    const COMMON: u32 = 0x0000;
    const NOTIFY: u32 = 0x1000;

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;
    let indirect = 0x4000u32;
    let buf = 0x4100u32;

    let len = (MAX_INDIRECT_DESC_TABLE_ENTRIES + 1) * 16;
    write_desc(
        &guest,
        desc_table,
        0,
        indirect as u64,
        len,
        VIRTQ_DESC_F_INDIRECT,
        0,
    );
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 1, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    guest.write_u16(avail, 0);
    guest.write_u16(avail + 2, 2);
    guest.write_u16(avail + 4, 0);
    guest.write_u16(avail + 6, 1);
    guest.write_u16(used, 0);
    guest.write_u16(used + 2, 0);

    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");

    let notify_off = bridge.mmio_read(COMMON + 0x1e, 2);
    bridge.mmio_write(
        NOTIFY + notify_off * 4,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

    assert_eq!(guest.read_u16(used + 2), 2);
    assert_eq!(guest.read_u32(used + 4), 0);
    assert_eq!(guest.read_u32(used + 8), 0);
    assert_eq!(guest.read_u32(used + 12), 1);
    assert_eq!(guest.read_u32(used + 16), 8);

    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_eventq_retains_event_when_first_chain_has_nested_indirect_descriptor() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    bridge.set_pci_command(0x0006);

    let _qsz = init_snd_eventq(&mut bridge);

    const COMMON: u32 = 0x0000;
    const NOTIFY: u32 = 0x1000;

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;
    let indirect_table = 0x4000u32;
    let buf = 0x4100u32;

    // Indirect head points to a 1-entry indirect table.
    write_desc(
        &guest,
        desc_table,
        0,
        indirect_table as u64,
        16,
        VIRTQ_DESC_F_INDIRECT,
        0,
    );
    // Indirect table entry itself is an indirect descriptor (nested indirect), which is forbidden.
    write_desc(&guest, indirect_table, 0, 0, 16, VIRTQ_DESC_F_INDIRECT, 0);

    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 1, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    guest.write_u16(avail, 0);
    guest.write_u16(avail + 2, 2);
    guest.write_u16(avail + 4, 0);
    guest.write_u16(avail + 6, 1);
    guest.write_u16(used, 0);
    guest.write_u16(used + 2, 0);

    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");

    let notify_off = bridge.mmio_read(COMMON + 0x1e, 2);
    bridge.mmio_write(
        NOTIFY + notify_off * 4,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

    assert_eq!(guest.read_u16(used + 2), 2);
    assert_eq!(guest.read_u32(used + 4), 0);
    assert_eq!(guest.read_u32(used + 8), 0);
    assert_eq!(guest.read_u32(used + 12), 1);
    assert_eq!(guest.read_u32(used + 16), 8);

    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());
}
