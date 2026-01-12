#![cfg(target_arch = "wasm32")]

use aero_virtio::devices::snd::{VIRTIO_SND_R_PCM_INFO, VIRTIO_SND_S_OK};
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_ISR_QUEUE, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use aero_wasm::VirtioSndPciBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn write_u16(mem: &mut [u8], addr: u32, value: u16) {
    let addr = addr as usize;
    mem[addr..addr + 2].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(mem: &[u8], addr: u32) -> u16 {
    let addr = addr as usize;
    u16::from_le_bytes([mem[addr], mem[addr + 1]])
}

fn write_u32(mem: &mut [u8], addr: u32, value: u32) {
    let addr = addr as usize;
    mem[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(mem: &mut [u8], addr: u32, value: u64) {
    let addr = addr as usize;
    mem[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_desc(mem: &mut [u8], table: u32, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u32::from(index) * 16;
    write_u64(mem, base, addr);
    write_u32(mem, base + 8, len);
    write_u16(mem, base + 12, flags);
    write_u16(mem, base + 14, next);
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_is_gated_on_pci_bus_master_enable() {
    // Synthetic guest RAM region outside the wasm heap.
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    let mut bridge =
        VirtioSndPciBridge::new(guest_base, guest_size, None).expect("VirtioSndPciBridge::new");
    // Enable PCI memory decoding so BAR0 MMIO reads/writes reach the device, but keep Bus Master
    // Enable disabled so the device cannot DMA until the guest explicitly enables it during PCI
    // enumeration.
    bridge.set_pci_command(0x0002);

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

    bridge.mmio_write(COMMON + 0x00, 4, 0); // device_feature_select
    let f0 = bridge.mmio_read(COMMON + 0x04, 4);
    bridge.mmio_write(COMMON + 0x08, 4, 0); // driver_feature_select
    bridge.mmio_write(COMMON + 0x0c, 4, f0); // driver_features

    bridge.mmio_write(COMMON + 0x00, 4, 1);
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

    // Configure control queue 0 (virtio-snd).
    bridge.mmio_write(COMMON + 0x16, 2, 0); // queue_select
    let qsz = bridge.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz >= 8);

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

    // Descriptor chain: [control request (out)][response buffer (in)].
    let req_addr = 0x4000u32;
    let resp_addr = 0x4100u32;
    let mut req = Vec::new();
    req.extend_from_slice(&VIRTIO_SND_R_PCM_INFO.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes()); // start_id
    req.extend_from_slice(&2u32.to_le_bytes()); // count
    guest[req_addr as usize..req_addr as usize + req.len()].copy_from_slice(&req);

    let resp_len = 128u32;
    guest[resp_addr as usize..resp_addr as usize + resp_len as usize].fill(0xAA);

    write_desc(
        guest,
        desc_table,
        0,
        req_addr as u64,
        req.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        guest,
        desc_table,
        1,
        resp_addr as u64,
        resp_len,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // avail.idx = 1, ring[0] = 0
    write_u16(guest, avail, 0);
    write_u16(guest, avail + 2, 1);
    write_u16(guest, avail + 4, 0);

    // used.idx = 0
    write_u16(guest, used, 0);
    write_u16(guest, used + 2, 0);

    assert!(!bridge.irq_asserted(), "irq should start deasserted");

    // Notify queue 0 while BME is disabled. notify_mult=4, queue_notify_off=0.
    bridge.mmio_write(NOTIFY + 0, 2, 0);
    bridge.poll();

    assert!(
        !bridge.irq_asserted(),
        "irq should remain deasserted while PCI bus mastering is disabled"
    );
    assert_eq!(
        read_u16(guest, used + 2),
        0,
        "used.idx should not advance without bus mastering"
    );
    assert_eq!(
        &guest[resp_addr as usize..resp_addr as usize + 4],
        &[0xAA; 4],
        "response header should not be DMA-written while PCI bus mastering is disabled"
    );

    // Enable bus mastering and retry: the pending notify should now be processed via DMA.
    bridge.set_pci_command(0x0006);
    bridge.poll();

    assert_eq!(
        read_u16(guest, used + 2),
        1,
        "expected used.idx to advance after enabling bus mastering"
    );
    let status = u32::from_le_bytes(
        guest[resp_addr as usize..resp_addr as usize + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        status, VIRTIO_SND_S_OK,
        "unexpected control response status"
    );
    assert!(
        bridge.irq_asserted(),
        "irq should assert after control request completion"
    );

    // Reading ISR clears the asserted interrupt.
    let isr = bridge.mmio_read(ISR, 1) as u8;
    assert_ne!(
        isr & VIRTIO_PCI_LEGACY_ISR_QUEUE,
        0,
        "expected ISR queue bit"
    );
    assert!(
        !bridge.irq_asserted(),
        "irq should deassert after ISR read-to-clear"
    );
}
