#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_ipc::ring::RingBuffer;
use aero_machine::{PcMachine, PcMachineConfig};
use memory::MemoryBus as _;

#[test]
fn pc_machine_e1000_tx_ring_pushes_frame_to_net_tx_ring_backend() {
    // Host rings (NET_TX is guest->host).
    let tx_ring = Arc::new(RingBuffer::new(16 * 1024));
    let rx_ring = Arc::new(RingBuffer::new(16 * 1024));

    let mut m = PcMachine::new_with_config(PcMachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_e1000: true,
        ..Default::default()
    })
    .unwrap();

    m.attach_l2_tunnel_rings(tx_ring.clone(), rx_ring);

    let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;

    // Enable PCI decoding and bus mastering so the E1000 can DMA.
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
    let tx_ring_base = 0x1000u64;
    let pkt_base = 0x2000u64;

    // Minimum Ethernet frame length: dst MAC (6) + src MAC (6) + ethertype (2).
    const MIN_L2_FRAME_LEN: usize = 14;
    let frame = vec![0x11u8; MIN_L2_FRAME_LEN];

    // Write packet bytes into guest RAM.
    m.platform_mut().memory.write_physical(pkt_base, &frame);

    // Legacy TX descriptor: buffer_addr + length + cmd(EOP|RS).
    let mut desc = [0u8; 16];
    desc[0..8].copy_from_slice(&pkt_base.to_le_bytes());
    desc[8..10].copy_from_slice(&(frame.len() as u16).to_le_bytes());
    desc[10] = 0; // CSO
    desc[11] = (1 << 0) | (1 << 3); // EOP|RS
    desc[12] = 0; // status
    desc[13] = 0; // CSS
    desc[14..16].copy_from_slice(&0u16.to_le_bytes());
    m.platform_mut().memory.write_physical(tx_ring_base, &desc);

    // Program E1000 TX registers over MMIO (BAR0).
    let mem = &mut m.platform_mut().memory;
    mem.write_u32(bar0_base + 0x3800, tx_ring_base as u32); // TDBAL
    mem.write_u32(bar0_base + 0x3804, 0); // TDBAH
    mem.write_u32(bar0_base + 0x3808, 16 * 4); // TDLEN (4 descriptors)
    mem.write_u32(bar0_base + 0x3810, 0); // TDH
    mem.write_u32(bar0_base + 0x3818, 0); // TDT
    mem.write_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN

    // Doorbell: advance tail to include descriptor 0.
    mem.write_u32(bar0_base + 0x3818, 1); // TDT = 1

    // Poll the machine networking bridge once. This should run DMA and push the produced frame
    // into the NET_TX ring.
    m.poll_network();

    assert_eq!(tx_ring.try_pop(), Ok(frame));
}
