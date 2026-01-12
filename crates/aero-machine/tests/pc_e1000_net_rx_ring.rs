#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_ipc::ring::{PopError, RingBuffer};
use aero_machine::{PcMachine, PcMachineConfig};
use memory::MemoryBus as _;

#[test]
fn pc_machine_net_rx_ring_backend_delivers_frame_into_e1000_rx_ring() {
    // Host rings (NET_RX is host->guest).
    let tx_ring = Arc::new(RingBuffer::new(16 * 1024));
    let rx_ring = Arc::new(RingBuffer::new(16 * 1024));

    let mut m = PcMachine::new_with_config(PcMachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_e1000: true,
        ..Default::default()
    })
    .unwrap();

    m.attach_l2_tunnel_rings(tx_ring, rx_ring.clone());

    let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;

    // Enable PCI MMIO decoding + bus mastering so the E1000 can DMA into guest memory.
    {
        let mut pci_cfg = m.platform_mut().pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let cfg = bus
            .device_config_mut(bdf)
            .expect("E1000 device missing from PCI bus");
        // bit1 = memory space, bit2 = bus master
        cfg.set_command(0x2 | 0x4);
    }

    let bar0_base = {
        let mut pci_cfg = m.platform_mut().pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        bus.device_config(bdf)
            .and_then(|cfg| cfg.bar_range(0))
            .expect("missing E1000 BAR0")
            .base
    };

    // Guest memory layout.
    let rx_ring_base = 0x3000u64;
    let rx_buf0 = 0x4000u64;
    let rx_buf1 = 0x5000u64;

    // Minimum Ethernet frame length: dst MAC (6) + src MAC (6) + ethertype (2).
    const MIN_L2_FRAME_LEN: usize = 14;
    let frame = vec![0x22u8; MIN_L2_FRAME_LEN];

    // Set up a 2-descriptor legacy RX ring; keep one descriptor free by setting RDT=1.
    let mut desc0 = [0u8; 16];
    desc0[0..8].copy_from_slice(&rx_buf0.to_le_bytes());
    let mut desc1 = [0u8; 16];
    desc1[0..8].copy_from_slice(&rx_buf1.to_le_bytes());

    let mem = &mut m.platform_mut().memory;
    mem.write_physical(rx_ring_base, &desc0);
    mem.write_physical(rx_ring_base + 16, &desc1);

    // Program E1000 RX registers over MMIO (BAR0).
    mem.write_u32(bar0_base + 0x2800, rx_ring_base as u32); // RDBAL
    mem.write_u32(bar0_base + 0x2804, 0); // RDBAH
    mem.write_u32(bar0_base + 0x2808, 16 * 2); // RDLEN (2 descriptors)
    mem.write_u32(bar0_base + 0x2810, 0); // RDH
    mem.write_u32(bar0_base + 0x2818, 1); // RDT (tail=1, leaving 1 descriptor available)
    mem.write_u32(bar0_base + 0x0100, 1 << 1); // RCTL.EN

    // Push a host->guest frame into the NET_RX ring; the backend should pop it and enqueue into
    // the E1000 RX ring on the next poll.
    rx_ring.try_push(&frame).unwrap();

    m.poll_network();

    // Verify descriptor 0 completed and guest memory contains the frame.
    let mut out = vec![0u8; frame.len()];
    m.platform_mut().memory.read_physical(rx_buf0, &mut out);
    assert_eq!(out, frame);

    let desc0_after = m
        .platform_mut()
        .memory
        .read_physical_u128(rx_ring_base)
        .to_le_bytes();
    // Unpack: buffer_addr(8) + length(2) + checksum(2) + status(1) + errors(1) + special(2)
    let length = u16::from_le_bytes(desc0_after[8..10].try_into().unwrap());
    let status = desc0_after[12];
    let errors = desc0_after[13];
    assert_eq!(length, frame.len() as u16);
    assert_eq!(status & 0x03, 0x03, "RX descriptor should have DD|EOP set");
    assert_eq!(errors, 0);

    // Ensure the NET_RX ring was drained.
    assert_eq!(rx_ring.try_pop(), Err(PopError::Empty));
}
