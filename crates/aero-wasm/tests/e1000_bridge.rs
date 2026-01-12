#![cfg(target_arch = "wasm32")]

use aero_net_e1000::{ICR_RXT0, ICR_TXDW, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
use aero_wasm::E1000Bridge;
use js_sys::Uint8Array;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn write_u64_le(mem: &mut [u8], addr: u32, value: u64) {
    let addr = addr as usize;
    mem[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_u16_le(mem: &mut [u8], addr: u32, value: u16) {
    let addr = addr as usize;
    mem[addr..addr + 2].copy_from_slice(&value.to_le_bytes());
}

/// Minimal legacy TX descriptor layout (16 bytes).
fn write_tx_desc(mem: &mut [u8], addr: u32, buf_addr: u64, len: u16, cmd: u8, status: u8) {
    write_u64_le(mem, addr, buf_addr);
    write_u16_le(mem, addr + 8, len);
    mem[(addr + 10) as usize] = 0; // cso
    mem[(addr + 11) as usize] = cmd;
    mem[(addr + 12) as usize] = status;
    mem[(addr + 13) as usize] = 0; // css
    write_u16_le(mem, addr + 14, 0); // special
}

/// Minimal legacy RX descriptor layout (16 bytes).
fn write_rx_desc(mem: &mut [u8], addr: u32, buf_addr: u64, status: u8) {
    write_u64_le(mem, addr, buf_addr);
    write_u16_le(mem, addr + 8, 0); // length
    write_u16_le(mem, addr + 10, 0); // checksum
    mem[(addr + 12) as usize] = status;
    mem[(addr + 13) as usize] = 0; // errors
    write_u16_le(mem, addr + 14, 0); // special
}

fn build_test_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst MAC
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src MAC
    frame.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype (IPv4)
    frame.extend_from_slice(payload);
    frame
}

#[wasm_bindgen_test]
fn e1000_bridge_smoke_tx_rx_and_bme_gating() {
    // Synthetic guest RAM region above the bounded `runtime_alloc` heap.
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x40_000);
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    let mut bridge = E1000Bridge::new(guest_base, guest_size, None).expect("E1000Bridge::new");

    // PCI Bus Master Enable (BME) should start disabled so the device model cannot DMA into guest
    // memory before the guest explicitly enables it during enumeration.

    // Enable interrupts for both RX and TX.
    bridge.mmio_write(0x00D0, 4, ICR_RXT0 | ICR_TXDW); // IMS

    // Configure TX ring: 4 descriptors at 0x1000.
    bridge.mmio_write(0x3800, 4, 0x1000); // TDBAL
    bridge.mmio_write(0x3804, 4, 0); // TDBAH
    bridge.mmio_write(0x3808, 4, 4 * 16); // TDLEN
    bridge.mmio_write(0x3810, 4, 0); // TDH
    bridge.mmio_write(0x3818, 4, 0); // TDT
    bridge.mmio_write(0x0400, 4, 1 << 1); // TCTL.EN

    // Configure RX ring: 2 descriptors at 0x2000.
    bridge.mmio_write(0x2800, 4, 0x2000); // RDBAL
    bridge.mmio_write(0x2804, 4, 0); // RDBAH
    bridge.mmio_write(0x2808, 4, 2 * 16); // RDLEN
    bridge.mmio_write(0x2810, 4, 0); // RDH
    bridge.mmio_write(0x2818, 4, 1); // RDT
    bridge.mmio_write(0x0100, 4, 1 << 1); // RCTL.EN (defaults to 2048 buffer)

    // Populate RX descriptors with guest buffers.
    write_rx_desc(guest, 0x2000, 0x3000, 0);
    write_rx_desc(guest, 0x2010, 0x3400, 0);

    // With BME disabled, host RX frames should be queued but must not DMA into guest memory yet.
    let pkt_in = build_test_frame(b"host->guest");
    guest[0x3000..0x3000 + pkt_in.len()].fill(0xAA);
    bridge.receive_frame(&Uint8Array::from(pkt_in.as_slice()));
    assert!(
        guest[0x3000..0x3000 + pkt_in.len()]
            .iter()
            .all(|&b| b == 0xAA),
        "RX buffer should not be written while PCI bus mastering is disabled"
    );

    // Guest TX: descriptor 0 points at packet buffer 0x4000.
    let pkt_out = build_test_frame(b"guest->host");
    guest[0x4000..0x4000 + pkt_out.len()].copy_from_slice(&pkt_out);
    write_tx_desc(
        guest,
        0x1000,
        0x4000,
        pkt_out.len() as u16,
        0b0000_1001, // EOP|RS
        0,
    );
    bridge.mmio_write(0x3818, 4, 1); // TDT

    // With BME disabled, poll should not DMA descriptors or emit host frames.
    bridge.poll();
    assert!(
        bridge.pop_tx_frame().is_none(),
        "expected no TX frame while PCI bus mastering is disabled"
    );

    // Enable Bus Mastering and retry.
    bridge.set_pci_command(0x0004);
    bridge.poll();

    let tx = bridge.pop_tx_frame().expect("TX frame");
    let mut tx_bytes = vec![0u8; tx.length() as usize];
    tx.copy_to(&mut tx_bytes);
    assert_eq!(tx_bytes, pkt_out);

    assert_eq!(&guest[0x3000..0x3000 + pkt_in.len()], pkt_in.as_slice());

    assert!(bridge.irq_level(), "expected IRQ asserted after TX/RX completion");
    let causes = bridge.mmio_read(0x00C0, 4);
    assert_eq!(causes & (ICR_TXDW | ICR_RXT0), ICR_TXDW | ICR_RXT0);
    assert!(!bridge.irq_level(), "expected IRQ deasserted after ICR read");

    // Invalid host RX frames should be ignored without touching guest memory or asserting IRQ.
    //
    // Use the second RX buffer so we can validate that the frame would have been DMA'd if it were
    // accepted.
    guest[0x3400..0x3400 + 16].fill(0xAA);
    bridge.receive_frame(&Uint8Array::new_with_length((MIN_L2_FRAME_LEN - 1) as u32));
    bridge.poll();
    assert_eq!(&guest[0x3400..0x3400 + 16], &[0xAA; 16]);
    assert!(
        !bridge.irq_level(),
        "expected no IRQ asserted after short frame is dropped"
    );

    bridge.receive_frame(&Uint8Array::new_with_length((MAX_L2_FRAME_LEN + 1) as u32));
    bridge.poll();
    assert_eq!(&guest[0x3400..0x3400 + 16], &[0xAA; 16]);
    assert!(
        !bridge.irq_level(),
        "expected no IRQ asserted after oversized frame is dropped"
    );

    // Valid host RX: deliver into the next available descriptor.
    let pkt_in2 = build_test_frame(b"host->guest2");
    bridge.receive_frame(&Uint8Array::from(pkt_in2.as_slice()));
    bridge.poll();
    assert_eq!(&guest[0x3400..0x3400 + pkt_in2.len()], pkt_in2.as_slice());

    assert!(bridge.irq_level(), "expected IRQ asserted after RX delivery");
    let causes = bridge.mmio_read(0x00C0, 4); // ICR (read clears)
    assert_eq!(causes & ICR_RXT0, ICR_RXT0);
    assert!(!bridge.irq_level(), "expected IRQ deasserted after ICR read");
}
