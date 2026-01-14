#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::snd::{
    JACK_ID_SPEAKER, VIRTIO_SND_EVT_JACK_CONNECTED, VIRTIO_SND_QUEUE_EVENT,
};
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_GUEST_FEATURES, VIRTIO_PCI_LEGACY_HOST_FEATURES,
    VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, VIRTIO_PCI_LEGACY_QUEUE_NUM, VIRTIO_PCI_LEGACY_QUEUE_PFN,
    VIRTIO_PCI_LEGACY_QUEUE_SEL, VIRTIO_PCI_LEGACY_STATUS, VIRTIO_PCI_LEGACY_VRING_ALIGN,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_WRITE;
use aero_wasm::VirtioSndPciBridge;
use wasm_bindgen::JsValue;
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

fn legacy_vring_layout(pfn: u32, qsz: u16) -> (u32, u32, u32) {
    let base = u64::from(pfn) << 12;
    let desc_table = base as u32;
    let avail = (base + 16 * u64::from(qsz)) as u32;
    let used_unaligned = u64::from(avail) + 4 + 2 * u64::from(qsz) + 2;
    let used = ((used_unaligned + VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)
        & !(VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)) as u32;
    (desc_table, avail, used)
}

fn setup_legacy_eventq(
    bridge: &mut VirtioSndPciBridge,
    guest: &common::GuestRegion,
    pfn: u32,
) -> (u16, u32, u32, u32) {
    // Legacy feature negotiation (low 32 bits only).
    let host_features = bridge.io_read(VIRTIO_PCI_LEGACY_HOST_FEATURES as u32, 4);
    bridge.io_write(VIRTIO_PCI_LEGACY_GUEST_FEATURES as u32, 4, host_features);

    // Set device status.
    bridge.io_write(
        VIRTIO_PCI_LEGACY_STATUS as u32,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK),
    );

    // Configure event queue 1 via legacy registers.
    bridge.io_write(
        VIRTIO_PCI_LEGACY_QUEUE_SEL as u32,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );
    let qsz = bridge.io_read(VIRTIO_PCI_LEGACY_QUEUE_NUM as u32, 2) as u16;
    assert!(qsz >= 2, "expected event queue size >= 2");

    // Program the queue at PFN.
    bridge.io_write(VIRTIO_PCI_LEGACY_QUEUE_PFN as u32, 4, pfn);

    let (desc_table, avail, used) = legacy_vring_layout(pfn, qsz);

    // Zero flags.
    guest.write_u16(avail, 0);
    guest.write_u16(used, 0);

    (qsz, desc_table, avail, used)
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_legacy_only_eventq_retains_event_when_first_chain_head_is_out_of_range() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, Some(JsValue::from_str("legacy")))
            .expect("VirtioSndPciBridge::new");
    // Enable legacy I/O decoding + bus mastering.
    bridge.set_pci_command(0x0005);

    let pfn = 1u32;
    let (qsz, desc_table, avail, used) = setup_legacy_eventq(&mut bridge, &guest, pfn);

    let buf = 0x4000u32;
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    // Publish two avail entries: an invalid head index (qsz) and then a valid buffer at desc 0.
    guest.write_u16(avail + 2, 2); // avail.idx = 2
    guest.write_u16(avail + 4, qsz); // avail.ring[0] = out-of-range head
    guest.write_u16(avail + 6, 0); // avail.ring[1] = desc 0 (valid)
    guest.write_u16(used + 2, 0);

    // Queue an event.
    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");

    bridge.io_write(
        VIRTIO_PCI_LEGACY_QUEUE_NOTIFY as u32,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

    assert_eq!(guest.read_u16(used + 2), 2);
    assert_eq!(guest.read_u32(used + 4), u32::from(qsz));
    assert_eq!(guest.read_u32(used + 8), 0);
    assert_eq!(guest.read_u32(used + 12), 0);
    assert_eq!(guest.read_u32(used + 16), 8);

    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_transitional_eventq_retains_event_when_first_chain_head_is_out_of_range_via_legacy_io()
 {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge = VirtioSndPciBridge::new(
        guest_base,
        guest_size,
        Some(JsValue::from_str("transitional")),
    )
    .expect("VirtioSndPciBridge::new");
    // Enable legacy I/O decoding + bus mastering.
    bridge.set_pci_command(0x0005);

    let pfn = 1u32;
    let (qsz, desc_table, avail, used) = setup_legacy_eventq(&mut bridge, &guest, pfn);

    let buf = 0x4000u32;
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    guest.write_u16(avail + 2, 2); // avail.idx = 2
    guest.write_u16(avail + 4, qsz); // avail.ring[0] = out-of-range head
    guest.write_u16(avail + 6, 0); // avail.ring[1] = desc 0 (valid)
    guest.write_u16(used + 2, 0);

    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");

    bridge.io_write(
        VIRTIO_PCI_LEGACY_QUEUE_NOTIFY as u32,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

    assert_eq!(guest.read_u16(used + 2), 2);
    assert_eq!(guest.read_u32(used + 4), u32::from(qsz));
    assert_eq!(guest.read_u32(used + 8), 0);
    assert_eq!(guest.read_u32(used + 12), 0);
    assert_eq!(guest.read_u32(used + 16), 8);

    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());
}
