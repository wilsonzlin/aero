#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::snd::{
    JACK_ID_SPEAKER, VIRTIO_SND_EVT_JACK_CONNECTED, VIRTIO_SND_QUEUE_EVENT,
};
use aero_virtio::pci::{
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_WRITE;
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

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_load_state_restores_pci_command_for_dma_gating() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge1 =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    // Enable MMIO decoding + bus mastering so the device can DMA.
    bridge1.set_pci_command(0x0006);

    const COMMON: u32 = 0x0000;
    const NOTIFY: u32 = 0x1000;

    // Minimal virtio feature negotiation (accept everything offered).
    bridge1.mmio_write(COMMON + 0x14, 1, u32::from(VIRTIO_STATUS_ACKNOWLEDGE));
    bridge1.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER),
    );

    bridge1.mmio_write(COMMON, 4, 0); // device_feature_select
    let f0 = bridge1.mmio_read(COMMON + 0x04, 4);
    bridge1.mmio_write(COMMON + 0x08, 4, 0); // driver_feature_select
    bridge1.mmio_write(COMMON + 0x0c, 4, f0); // driver_features

    bridge1.mmio_write(COMMON, 4, 1);
    let f1 = bridge1.mmio_read(COMMON + 0x04, 4);
    bridge1.mmio_write(COMMON + 0x08, 4, 1);
    bridge1.mmio_write(COMMON + 0x0c, 4, f1);

    bridge1.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK),
    );
    bridge1.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        ),
    );

    // Configure event queue 1.
    bridge1.mmio_write(COMMON + 0x16, 2, u32::from(VIRTIO_SND_QUEUE_EVENT)); // queue_select
    let qsz = bridge1.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz >= 1, "expected event queue size >= 1");

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;
    let buf = 0x4000u32;

    bridge1.mmio_write(COMMON + 0x20, 4, desc_table);
    bridge1.mmio_write(COMMON + 0x24, 4, 0);
    bridge1.mmio_write(COMMON + 0x28, 4, avail);
    bridge1.mmio_write(COMMON + 0x2c, 4, 0);
    bridge1.mmio_write(COMMON + 0x30, 4, used);
    bridge1.mmio_write(COMMON + 0x34, 4, 0);
    bridge1.mmio_write(COMMON + 0x1c, 2, 1); // queue_enable

    // Post a writable event buffer and notify while no events are queued, so the buffer is cached.
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);
    guest.write_u16(avail, 0);
    guest.write_u16(avail + 2, 1);
    guest.write_u16(avail + 4, 0);
    guest.write_u16(used, 0);
    guest.write_u16(used + 2, 0);

    let notify_off = bridge1.mmio_read(COMMON + 0x1e, 2);
    bridge1.mmio_write(
        NOTIFY + notify_off * 4,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

    assert_eq!(
        guest.read_u16(used + 2),
        0,
        "without queued events, the event buffer should remain cached (no used entry)"
    );

    // Snapshot with BME enabled (PCI COMMAND.BME=1).
    let snap = bridge1.save_state();

    // Restore into a fresh bridge without calling `set_pci_command`. `VirtioSndPciBridge::load_state`
    // must mirror the restored PCI command register into its wrapper field so `poll()` performs DMA.
    let mut bridge2 =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    bridge2.load_state(&snap).expect("load_state");

    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge2
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");
    bridge2.poll();

    assert_eq!(guest.read_u16(used + 2), 1);
    assert_eq!(guest.read_u32(used + 8), 8);

    let expected_connected = {
        let mut evt = [0u8; 8];
        evt[0..4].copy_from_slice(&VIRTIO_SND_EVT_JACK_CONNECTED.to_le_bytes());
        evt[4..8].copy_from_slice(&JACK_ID_SPEAKER.to_le_bytes());
        evt
    };
    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_connected);
}
