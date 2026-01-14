#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::snd::{
    JACK_ID_SPEAKER, VIRTIO_SND_EVT_JACK_CONNECTED, VIRTIO_SND_QUEUE_EVENT,
};
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_GUEST_FEATURES, VIRTIO_PCI_LEGACY_HOST_FEATURES, VIRTIO_PCI_LEGACY_ISR,
    VIRTIO_PCI_LEGACY_ISR_QUEUE, VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, VIRTIO_PCI_LEGACY_QUEUE_NUM,
    VIRTIO_PCI_LEGACY_QUEUE_PFN, VIRTIO_PCI_LEGACY_QUEUE_SEL, VIRTIO_PCI_LEGACY_STATUS,
    VIRTIO_PCI_LEGACY_VRING_ALIGN, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK,
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

fn legacy_vring_addresses(pfn: u32, qsz: u16) -> (u32, u32, u32) {
    let base = u64::from(pfn) << 12;
    let desc_table = base as u32;
    let avail = (base + 16 * u64::from(qsz)) as u32;
    let used_unaligned = u64::from(avail) + 4 + 2 * u64::from(qsz) + 2;
    let used = ((used_unaligned + VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)
        & !(VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)) as u32;
    (desc_table, avail, used)
}

fn expected_speaker_connected() -> [u8; 8] {
    let mut evt = [0u8; 8];
    evt[0..4].copy_from_slice(&VIRTIO_SND_EVT_JACK_CONNECTED.to_le_bytes());
    evt[4..8].copy_from_slice(&JACK_ID_SPEAKER.to_le_bytes());
    evt
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_legacy_only_eventq_intx_disable_suppresses_line_but_retains_pending_interrupt()
 {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, Some(JsValue::from_str("legacy")))
            .expect("VirtioSndPciBridge::new");
    // Enable legacy I/O decoding + bus mastering, but disable INTx delivery.
    bridge.set_pci_command(0x0405);

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
    assert!(qsz >= 1);

    // Program the queue at PFN=1 (base 0x1000).
    let pfn = 1u32;
    bridge.io_write(VIRTIO_PCI_LEGACY_QUEUE_PFN as u32, 4, pfn);
    let (desc_table, avail, used) = legacy_vring_addresses(pfn, qsz);

    // Post one event buffer.
    let buf = 0x4000u32;
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);
    guest.write_u16(avail, 0);
    guest.write_u16(avail + 2, 1);
    guest.write_u16(avail + 4, 0);
    guest.write_u16(used, 0);
    guest.write_u16(used + 2, 0);

    // Queue an event.
    let ring = WorkletBridge::new(8, 2).unwrap();
    let sab = ring.shared_buffer();
    bridge
        .set_audio_ring_buffer(Some(sab), 8, 2)
        .expect("set_audio_ring_buffer(Some)");

    // Kick the queue: should complete the buffer and latch an interrupt, but not assert INTx.
    bridge.io_write(
        VIRTIO_PCI_LEGACY_QUEUE_NOTIFY as u32,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

    assert_eq!(guest.read_u16(used + 2), 1);
    assert_eq!(guest.read_u32(used + 8), 8);
    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());
    assert!(
        !bridge.irq_asserted(),
        "INTx line should remain deasserted while PCI COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx delivery: the pending interrupt should now assert.
    bridge.set_pci_command(0x0005);
    assert!(
        bridge.irq_asserted(),
        "pending INTx interrupt should assert once INTX_DISABLE is cleared"
    );

    // Reading ISR clears the interrupt latch.
    let isr = bridge.io_read(VIRTIO_PCI_LEGACY_ISR as u32, 1) as u8;
    assert_ne!(isr & VIRTIO_PCI_LEGACY_ISR_QUEUE, 0);
    assert!(!bridge.irq_asserted());
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_transitional_eventq_intx_disable_suppresses_line_but_retains_pending_interrupt_via_legacy_io()
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
    // Enable legacy I/O decoding + bus mastering, but disable INTx delivery.
    bridge.set_pci_command(0x0405);

    let host_features = bridge.io_read(VIRTIO_PCI_LEGACY_HOST_FEATURES as u32, 4);
    bridge.io_write(VIRTIO_PCI_LEGACY_GUEST_FEATURES as u32, 4, host_features);

    bridge.io_write(
        VIRTIO_PCI_LEGACY_STATUS as u32,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK),
    );

    bridge.io_write(
        VIRTIO_PCI_LEGACY_QUEUE_SEL as u32,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );
    let qsz = bridge.io_read(VIRTIO_PCI_LEGACY_QUEUE_NUM as u32, 2) as u16;
    assert!(qsz >= 1);

    let pfn = 1u32;
    bridge.io_write(VIRTIO_PCI_LEGACY_QUEUE_PFN as u32, 4, pfn);
    let (desc_table, avail, used) = legacy_vring_addresses(pfn, qsz);

    let buf = 0x4000u32;
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);
    guest.write_u16(avail, 0);
    guest.write_u16(avail + 2, 1);
    guest.write_u16(avail + 4, 0);
    guest.write_u16(used, 0);
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

    assert_eq!(guest.read_u16(used + 2), 1);
    assert_eq!(guest.read_u32(used + 8), 8);
    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());
    assert!(
        !bridge.irq_asserted(),
        "INTx line should remain deasserted while PCI COMMAND.INTX_DISABLE is set"
    );

    bridge.set_pci_command(0x0005);
    assert!(bridge.irq_asserted());

    let isr = bridge.io_read(VIRTIO_PCI_LEGACY_ISR as u32, 1) as u8;
    assert_ne!(isr & VIRTIO_PCI_LEGACY_ISR_QUEUE, 0);
    assert!(!bridge.irq_asserted());
}
