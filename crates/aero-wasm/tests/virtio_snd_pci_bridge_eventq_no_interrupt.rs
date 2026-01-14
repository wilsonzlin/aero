#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::snd::{
    JACK_ID_SPEAKER, VIRTIO_SND_EVT_JACK_CONNECTED, VIRTIO_SND_QUEUE_EVENT,
};
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_GUEST_FEATURES, VIRTIO_PCI_LEGACY_HOST_FEATURES,
    VIRTIO_PCI_LEGACY_ISR,
    VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, VIRTIO_PCI_LEGACY_QUEUE_NUM, VIRTIO_PCI_LEGACY_QUEUE_PFN,
    VIRTIO_PCI_LEGACY_QUEUE_SEL, VIRTIO_PCI_LEGACY_STATUS, VIRTIO_PCI_LEGACY_VRING_ALIGN,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_AVAIL_F_NO_INTERRUPT, VIRTQ_DESC_F_WRITE};
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

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_eventq_does_not_raise_irq_when_avail_no_interrupt_is_set() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    bridge.set_pci_command(0x0006);

    const COMMON: u32 = 0x0000;
    const NOTIFY: u32 = 0x1000;
    const ISR: u32 = 0x2000;

    // Minimal virtio feature negotiation (accept everything offered).
    bridge.mmio_write(COMMON + 0x14, 1, u32::from(VIRTIO_STATUS_ACKNOWLEDGE));
    bridge.mmio_write(
        COMMON + 0x14,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER),
    );

    bridge.mmio_write(COMMON, 4, 0);
    let f0 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 0);
    bridge.mmio_write(COMMON + 0x0c, 4, f0);

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

    // Configure event queue 1.
    bridge.mmio_write(COMMON + 0x16, 2, u32::from(VIRTIO_SND_QUEUE_EVENT));
    let qsz = bridge.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz >= 1);

    let desc_table = 0x1000u32;
    let avail = 0x2000u32;
    let used = 0x3000u32;
    let buf = 0x4000u32;

    bridge.mmio_write(COMMON + 0x20, 4, desc_table);
    bridge.mmio_write(COMMON + 0x24, 4, 0);
    bridge.mmio_write(COMMON + 0x28, 4, avail);
    bridge.mmio_write(COMMON + 0x2c, 4, 0);
    bridge.mmio_write(COMMON + 0x30, 4, used);
    bridge.mmio_write(COMMON + 0x34, 4, 0);
    bridge.mmio_write(COMMON + 0x1c, 2, 1);

    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    // avail.flags = NO_INTERRUPT, avail.idx = 1, ring[0] = 0.
    guest.write_u16(avail, VIRTQ_AVAIL_F_NO_INTERRUPT);
    guest.write_u16(avail + 2, 1);
    guest.write_u16(avail + 4, 0);
    guest.write_u16(used, 0);
    guest.write_u16(used + 2, 0);

    // Queue an event and kick eventq.
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

    assert_eq!(guest.read_u16(used + 2), 1);
    assert_eq!(guest.read_u32(used + 8), 8);

    let mut got_evt = [0u8; 8];
    guest.read_into(buf, &mut got_evt);
    assert_eq!(&got_evt, &expected_speaker_connected());

    assert!(
        !bridge.irq_asserted(),
        "IRQ should remain deasserted when VIRTQ_AVAIL_F_NO_INTERRUPT is set"
    );

    // No interrupt should be pending in the ISR.
    let isr = bridge.mmio_read(ISR, 1) as u8;
    assert_eq!(isr, 0);
    assert!(!bridge.irq_asserted());
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_legacy_only_eventq_does_not_raise_irq_when_avail_no_interrupt_is_set() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, Some(JsValue::from_str("legacy")))
            .expect("VirtioSndPciBridge::new");
    bridge.set_pci_command(0x0005);

    // Legacy feature negotiation (low 32 bits only).
    let host_features = bridge.io_read(VIRTIO_PCI_LEGACY_HOST_FEATURES as u32, 4);
    bridge.io_write(VIRTIO_PCI_LEGACY_GUEST_FEATURES as u32, 4, host_features);

    // Set device status.
    bridge.io_write(
        VIRTIO_PCI_LEGACY_STATUS as u32,
        1,
        u32::from(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK),
    );

    // Configure event queue 1.
    bridge.io_write(
        VIRTIO_PCI_LEGACY_QUEUE_SEL as u32,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );
    let qsz = bridge.io_read(VIRTIO_PCI_LEGACY_QUEUE_NUM as u32, 2) as u16;
    assert!(qsz >= 1);

    let pfn = 1u32;
    bridge.io_write(VIRTIO_PCI_LEGACY_QUEUE_PFN as u32, 4, pfn);

    let base = u64::from(pfn) << 12;
    let desc_table = base as u32;
    let avail = (base + 16 * u64::from(qsz)) as u32;
    let used_unaligned = u64::from(avail) + 4 + 2 * u64::from(qsz) + 2;
    let used = ((used_unaligned + VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)
        & !(VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)) as u32;

    let buf = 0x4000u32;
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    // avail.flags = NO_INTERRUPT, avail.idx = 1, ring[0] = 0.
    guest.write_u16(avail, VIRTQ_AVAIL_F_NO_INTERRUPT);
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
        "IRQ should remain deasserted when VIRTQ_AVAIL_F_NO_INTERRUPT is set"
    );

    let isr = bridge.io_read(VIRTIO_PCI_LEGACY_ISR as u32, 1) as u8;
    assert_eq!(isr, 0);
    assert!(!bridge.irq_asserted());
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_transitional_eventq_does_not_raise_irq_when_avail_no_interrupt_is_set_via_legacy_io()
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
    bridge.set_pci_command(0x0005);

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

    let base = u64::from(pfn) << 12;
    let desc_table = base as u32;
    let avail = (base + 16 * u64::from(qsz)) as u32;
    let used_unaligned = u64::from(avail) + 4 + 2 * u64::from(qsz) + 2;
    let used = ((used_unaligned + VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)
        & !(VIRTIO_PCI_LEGACY_VRING_ALIGN - 1)) as u32;

    let buf = 0x4000u32;
    guest.fill(buf, 8, 0xAA);
    write_desc(&guest, desc_table, 0, buf as u64, 8, VIRTQ_DESC_F_WRITE, 0);

    guest.write_u16(avail, VIRTQ_AVAIL_F_NO_INTERRUPT);
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
        "IRQ should remain deasserted when VIRTQ_AVAIL_F_NO_INTERRUPT is set"
    );

    let isr = bridge.io_read(VIRTIO_PCI_LEGACY_ISR as u32, 1) as u8;
    assert_eq!(isr, 0);
    assert!(!bridge.irq_asserted());
}
