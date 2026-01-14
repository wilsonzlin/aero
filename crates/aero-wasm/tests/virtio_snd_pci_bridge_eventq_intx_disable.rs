#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::snd::{
    JACK_ID_SPEAKER, VIRTIO_SND_EVT_JACK_CONNECTED, VIRTIO_SND_QUEUE_EVENT,
};
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_ISR_QUEUE, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
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
fn virtio_snd_pci_bridge_eventq_intx_disable_suppresses_line_but_retains_pending_interrupt() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    // Enable MMIO decoding + bus mastering, but disable legacy INTx delivery (PCI COMMAND.INTX_DISABLE).
    bridge.set_pci_command(0x0406);

    // BAR0 layout is fixed by `aero_virtio::pci::VirtioPciDevice`.
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

    let notify_off = bridge.mmio_read(COMMON + 0x1e, 2);
    bridge.mmio_write(
        NOTIFY + notify_off * 4,
        2,
        u32::from(VIRTIO_SND_QUEUE_EVENT),
    );

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

    assert!(
        !bridge.irq_asserted(),
        "INTx line should remain deasserted while PCI COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx delivery: the pending interrupt should now assert.
    bridge.set_pci_command(0x0006);
    assert!(
        bridge.irq_asserted(),
        "pending INTx interrupt should assert once INTX_DISABLE is cleared"
    );

    // Reading ISR clears the interrupt latch.
    let isr = bridge.mmio_read(ISR, 1) as u8;
    assert_ne!(isr & VIRTIO_PCI_LEGACY_ISR_QUEUE, 0);
    assert!(!bridge.irq_asserted());
}

