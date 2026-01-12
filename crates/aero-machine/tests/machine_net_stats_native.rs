#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_ipc::ring::{PopError, RingBuffer};
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::L2TunnelRingBackendStats;
use aero_net_e1000::MIN_L2_FRAME_LEN;

#[test]
fn machine_e1000_l2_tunnel_rings_tx_rx_stats_smoke() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_e1000: true,
        enable_vga: false,
        // Keep the machine minimal and deterministic for this unit test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .expect("Machine::new");

    // No backend attached yet.
    assert!(m.network_backend_l2_ring_stats().is_none());

    let tx_ring = Arc::new(RingBuffer::new(16 * 1024));
    let rx_ring = Arc::new(RingBuffer::new(16 * 1024));
    m.attach_l2_tunnel_rings(tx_ring.clone(), rx_ring.clone());

    // Initial stats should be present and zeroed.
    assert_eq!(
        m.network_backend_l2_ring_stats(),
        Some(L2TunnelRingBackendStats::default())
    );

    let pci_cfg = m
        .pci_config_ports()
        .expect("pc platform enabled => pci_config_ports is present");
    let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;

    // Enable PCI memory decoding + bus mastering so the E1000 can DMA.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("E1000 device missing from PCI bus");
        cfg.set_command(0x2 | 0x4);
    }

    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .device_config(bdf)
            .and_then(|cfg| cfg.bar_range(0))
            .expect("missing E1000 BAR0")
            .base
    };

    // --------------------------------
    // Guest -> host (TX ring -> NET_TX)
    // --------------------------------
    let tx_desc_base: u64 = 0x20_000;
    let tx_buf: u64 = 0x21_000;
    let tx_frame: Vec<u8> = (0..MIN_L2_FRAME_LEN).map(|i| i as u8).collect();
    m.write_physical(tx_buf, &tx_frame);

    // Legacy TX descriptor (16 bytes): buffer addr + length + cmd(EOP|RS).
    let mut tx_desc = [0u8; 16];
    tx_desc[0..8].copy_from_slice(&tx_buf.to_le_bytes());
    tx_desc[8..10].copy_from_slice(&(tx_frame.len() as u16).to_le_bytes());
    tx_desc[11] = 0x01 | 0x08; // EOP | RS
    m.write_physical(tx_desc_base, &tx_desc);

    // Program TX ring registers over MMIO (BAR0).
    m.write_physical_u32(bar0_base + 0x3800, tx_desc_base as u32); // TDBAL
    m.write_physical_u32(bar0_base + 0x3804, 0);
    m.write_physical_u32(bar0_base + 0x3808, 16 * 4); // 4 descriptors
    m.write_physical_u32(bar0_base + 0x3810, 0);
    m.write_physical_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN
    m.write_physical_u32(bar0_base + 0x3818, 1); // TDT = 1 (one desc ready)

    // --------------------------------
    // Host -> guest (NET_RX -> RX DMA)
    // --------------------------------
    let rx_desc_base: u64 = 0x22_000;
    let rx_buf: u64 = 0x23_000;

    // Two RX descriptors (capacity 1 due to head==tail full/empty rule).
    let mut rx_desc0 = [0u8; 16];
    rx_desc0[0..8].copy_from_slice(&rx_buf.to_le_bytes());
    m.write_physical(rx_desc_base, &rx_desc0);
    m.write_physical(rx_desc_base + 16, &[0u8; 16]); // unused desc1

    m.write_physical_u32(bar0_base + 0x2800, rx_desc_base as u32); // RDBAL
    m.write_physical_u32(bar0_base + 0x2804, 0);
    m.write_physical_u32(bar0_base + 0x2808, 16 * 2); // 2 descriptors
    m.write_physical_u32(bar0_base + 0x2810, 0); // RDH
    m.write_physical_u32(bar0_base + 0x2818, 1); // RDT
    m.write_physical_u32(bar0_base + 0x0100, 1 << 1); // RCTL.EN

    let rx_frame: Vec<u8> = (0..MIN_L2_FRAME_LEN).rev().map(|i| i as u8).collect();
    m.write_physical(rx_buf, &vec![0xaa; rx_frame.len()]);
    rx_ring
        .try_push(&rx_frame)
        .expect("NET_RX ring try_push should succeed");

    // Run one network pump iteration. This should:
    // - DMA TX descriptors and push the produced frame to NET_TX.
    // - Pop one frame from NET_RX and DMA it into the guest RX ring.
    m.poll_network();

    assert_eq!(tx_ring.try_pop(), Ok(tx_frame.clone()));
    assert_eq!(tx_ring.try_pop(), Err(PopError::Empty));

    let tx_desc_after = m.read_physical_bytes(tx_desc_base, 16);
    assert_eq!(
        tx_desc_after[12] & 0x01,
        0x01,
        "TX descriptor DD should be set"
    );

    assert_eq!(m.read_physical_bytes(rx_buf, rx_frame.len()), rx_frame);

    let rx_desc_after = m.read_physical_bytes(rx_desc_base, 16);
    let rx_len = u16::from_le_bytes([rx_desc_after[8], rx_desc_after[9]]) as usize;
    assert_eq!(rx_len, rx_frame.len());
    assert_eq!(
        rx_desc_after[12] & 0x03,
        0x03,
        "RX descriptor should have DD|EOP set"
    );

    assert_eq!(rx_ring.try_pop(), Err(PopError::Empty));

    let stats = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats.tx_pushed_frames, 1);
    assert_eq!(stats.tx_dropped_oversize, 0);
    assert_eq!(stats.tx_dropped_full, 0);
    assert_eq!(stats.rx_popped_frames, 1);
    assert_eq!(stats.rx_dropped_oversize, 0);
    assert_eq!(stats.rx_corrupt, 0);
}
