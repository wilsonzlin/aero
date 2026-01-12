#![cfg(target_arch = "wasm32")]

use aero_ipc::ipc::{IpcQueueSpec, create_ipc_buffer};
use aero_ipc::layout::io_ipc_queue_kind::{NET_RX, NET_TX};
use aero_ipc::wasm::open_ring_by_kind;
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::pci::{
    VIRTIO_PCI_LEGACY_ISR_QUEUE, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_NEXT;
use aero_wasm::VirtioNetPciBridge;
use js_sys::{SharedArrayBuffer, Uint8Array};
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn make_io_ipc_sab() -> SharedArrayBuffer {
    let bytes = create_ipc_buffer(&[
        IpcQueueSpec {
            kind: NET_TX,
            capacity_bytes: 4096,
        },
        IpcQueueSpec {
            kind: NET_RX,
            capacity_bytes: 4096,
        },
    ]);

    let sab = SharedArrayBuffer::new(bytes.len() as u32);
    let view = Uint8Array::new(&sab);
    view.copy_from(&bytes);
    sab
}

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
fn virtio_net_pci_bridge_smoke_and_irq_latch() {
    // Synthetic guest RAM region outside the wasm heap.
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20000);
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    let io_ipc_sab = make_io_ipc_sab();
    let net_tx_ring = open_ring_by_kind(io_ipc_sab.clone(), NET_TX, 0).expect("open NET_TX ring");
    let net_rx_ring = open_ring_by_kind(io_ipc_sab.clone(), NET_RX, 0).expect("open NET_RX ring");
    let mut bridge = VirtioNetPciBridge::new(guest_base, guest_size, io_ipc_sab, None)
        .expect("VirtioNetPciBridge::new");
    // PCI Bus Master Enable should start disabled to prevent DMA before the guest explicitly
    // enables it during PCI enumeration.

    // Unknown BAR0 reads should return 0.
    assert_eq!(bridge.mmio_read(0x500, 4), 0);

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

    // Configure RX queue 0.
    bridge.mmio_write(COMMON + 0x16, 2, 0); // queue_select
    let qsz_rx = bridge.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz_rx >= 8);

    let rx_desc = 0x1000u32;
    let rx_avail = 0x2000u32;
    let rx_used = 0x3000u32;

    bridge.mmio_write(COMMON + 0x20, 4, rx_desc);
    bridge.mmio_write(COMMON + 0x24, 4, 0);
    bridge.mmio_write(COMMON + 0x28, 4, rx_avail);
    bridge.mmio_write(COMMON + 0x2c, 4, 0);
    bridge.mmio_write(COMMON + 0x30, 4, rx_used);
    bridge.mmio_write(COMMON + 0x34, 4, 0);
    bridge.mmio_write(COMMON + 0x1c, 2, 1); // queue_enable

    // Configure TX queue 1.
    bridge.mmio_write(COMMON + 0x16, 2, 1); // queue_select
    let qsz = bridge.mmio_read(COMMON + 0x18, 2) as u16;
    assert!(qsz >= 8);

    let tx_desc = 0x4000u32;
    let tx_avail = 0x5000u32;
    let tx_used = 0x6000u32;

    // queue_desc (low/high), queue_avail, queue_used.
    bridge.mmio_write(COMMON + 0x20, 4, tx_desc);
    bridge.mmio_write(COMMON + 0x24, 4, 0);
    bridge.mmio_write(COMMON + 0x28, 4, tx_avail);
    bridge.mmio_write(COMMON + 0x2c, 4, 0);
    bridge.mmio_write(COMMON + 0x30, 4, tx_used);
    bridge.mmio_write(COMMON + 0x34, 4, 0);

    bridge.mmio_write(COMMON + 0x1c, 2, 1); // queue_enable

    // Post a single TX descriptor chain: [virtio_net_hdr][ethernet frame].
    let hdr_addr = 0x7000u32;
    let payload_addr = 0x7100u32;
    guest[hdr_addr as usize..hdr_addr as usize + VirtioNetHdr::BASE_LEN].fill(0);
    let payload = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\x08\x00";
    guest[payload_addr as usize..payload_addr as usize + payload.len()].copy_from_slice(payload);

    write_desc(
        guest,
        tx_desc,
        0,
        hdr_addr as u64,
        VirtioNetHdr::BASE_LEN as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        guest,
        tx_desc,
        1,
        payload_addr as u64,
        payload.len() as u32,
        0,
        0,
    );

    // avail.idx = 1, ring[0] = 0
    write_u16(guest, tx_avail, 0);
    write_u16(guest, tx_avail + 2, 1);
    write_u16(guest, tx_avail + 4, 0);

    // used.idx = 0
    write_u16(guest, tx_used, 0);
    write_u16(guest, tx_used + 2, 0);

    // Post a single RX descriptor chain: [virtio_net_hdr(write)][payload(write)].
    let rx_hdr_addr = 0x7200u32;
    let rx_payload_addr = 0x7300u32;
    guest[rx_hdr_addr as usize..rx_hdr_addr as usize + VirtioNetHdr::BASE_LEN].fill(0xAA);
    guest[rx_payload_addr as usize..rx_payload_addr as usize + 64].fill(0xBB);

    write_desc(
        guest,
        rx_desc,
        0,
        rx_hdr_addr as u64,
        VirtioNetHdr::BASE_LEN as u32,
        VIRTQ_DESC_F_NEXT | aero_virtio::queue::VIRTQ_DESC_F_WRITE,
        1,
    );
    write_desc(
        guest,
        rx_desc,
        1,
        rx_payload_addr as u64,
        64,
        aero_virtio::queue::VIRTQ_DESC_F_WRITE,
        0,
    );

    // avail.idx = 1, ring[0] = 0
    write_u16(guest, rx_avail, 0);
    write_u16(guest, rx_avail + 2, 1);
    write_u16(guest, rx_avail + 4, 0);
    // used.idx = 0
    write_u16(guest, rx_used, 0);
    write_u16(guest, rx_used + 2, 0);

    // Inject one host->guest frame into the NET_RX ring while BME is disabled.
    let rx_frame = b"\xaa\xbb\xcc\xdd\xee\xff\x00\x01\x02\x03\x04\x05\x08\x00";
    assert!(net_rx_ring.try_push(rx_frame), "push host RX frame");

    assert!(!bridge.irq_asserted(), "irq should start deasserted");

    // Notify queue 1: notify base + notify_mult*1 (notify_mult=4).
    bridge.mmio_write(NOTIFY + 4, 2, 1);
    // Modern virtio-pci transport records the notify and defers queue processing until the
    // platform integration services queues via `poll()`/`process_notified_queues()`.
    bridge.poll();

    assert!(
        !bridge.irq_asserted(),
        "irq should remain deasserted while PCI bus mastering is disabled"
    );
    assert!(
        net_tx_ring.try_pop().is_none(),
        "NET_TX ring must remain empty while PCI bus mastering is disabled"
    );
    assert_eq!(
        read_u16(guest, tx_used + 2),
        0,
        "used.idx should not advance without bus mastering"
    );
    assert_eq!(
        &guest[rx_payload_addr as usize..rx_payload_addr as usize + rx_frame.len()],
        &[0xBB; 14],
        "RX payload buffer should not be DMA-written while PCI bus mastering is disabled"
    );

    // Enable bus mastering and retry: the pending notify should now be processed via DMA.
    bridge.set_pci_command(0x0004);
    bridge.poll();

    assert_eq!(
        read_u16(guest, tx_used + 2),
        1,
        "expected used.idx to advance"
    );
    assert_eq!(
        read_u16(guest, rx_used + 2),
        1,
        "expected RX used.idx to advance"
    );
    let tx = net_tx_ring
        .try_pop()
        .expect("expected one transmitted frame in NET_TX ring after enabling BME");
    let mut tx_bytes = vec![0u8; tx.length() as usize];
    tx.copy_to(&mut tx_bytes);
    assert_eq!(tx_bytes.as_slice(), payload, "TX payload mismatch");
    assert!(
        net_tx_ring.try_pop().is_none(),
        "expected NET_TX ring to contain exactly one frame"
    );
    assert!(
        net_rx_ring.try_pop().is_none(),
        "expected NET_RX ring to be drained after enabling BME"
    );
    assert_eq!(
        &guest[rx_hdr_addr as usize..rx_hdr_addr as usize + VirtioNetHdr::BASE_LEN],
        &[0u8; VirtioNetHdr::BASE_LEN],
        "expected virtio-net RX header to be zeroed by device"
    );
    assert_eq!(
        &guest[rx_payload_addr as usize..rx_payload_addr as usize + rx_frame.len()],
        rx_frame,
        "expected host RX frame to be DMA-written after enabling BME"
    );
    assert!(
        bridge.irq_asserted(),
        "irq should assert after TX completion"
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
