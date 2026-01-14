#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_ipc::ring::{PopError, PushError, RingBuffer};
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::L2TunnelRingBackendStats;
use aero_net_e1000::MIN_L2_FRAME_LEN;
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::pci::{
    VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG,
    VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    isr: u64,
    device: u64,
    notify_mult: u32,
}

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1f) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xfc)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn read_config_space_256(m: &mut Machine, bdf: PciBdf) -> [u8; 256] {
    let mut out = [0u8; 256];
    for off in (0..256u16).step_by(4) {
        let v = cfg_read(m, bdf, off, 4);
        out[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn parse_caps(cfg: &[u8; 256]) -> Caps {
    let mut caps = Caps::default();
    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        let cap_id = cfg[ptr];
        let next = cfg[ptr + 1] as usize;
        if cap_id == 0x09 {
            let cfg_type = cfg[ptr + 3];
            let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    caps.notify = offset;
                    caps.notify_mult =
                        u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
                }
                VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
                VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
                _ => {}
            }
        }
        ptr = next;
    }
    caps
}

fn write_desc(m: &mut Machine, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, next);
}

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

    // Keep the rings small so tests that intentionally fill them (to validate drop counters)
    // complete quickly.
    let tx_ring = Arc::new(RingBuffer::new(256));
    let rx_ring = Arc::new(RingBuffer::new(4 * 1024));
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
    assert_eq!(stats.tx_pushed_bytes, tx_frame.len() as u64);
    assert_eq!(stats.tx_dropped_oversize, 0);
    assert_eq!(stats.tx_dropped_oversize_bytes, 0);
    assert_eq!(stats.tx_dropped_full, 0);
    assert_eq!(stats.tx_dropped_full_bytes, 0);
    assert_eq!(stats.rx_popped_frames, 1);
    assert_eq!(stats.rx_popped_bytes, rx_frame.len() as u64);
    assert_eq!(stats.rx_dropped_oversize, 0);
    assert_eq!(stats.rx_dropped_oversize_bytes, 0);
    assert_eq!(stats.rx_corrupt, 0);

    // --------------------------------
    // Guest -> host drop when NET_TX ring is full
    // --------------------------------
    // Fill the NET_TX ring directly (bypassing the backend stats) so subsequent guest TX is forced
    // to hit `PushError::Full` inside `L2TunnelRingBackend::transmit`.
    let dummy_frame = vec![0x55u8; MIN_L2_FRAME_LEN];
    let mut filled = 0usize;
    loop {
        match tx_ring.try_push(&dummy_frame) {
            Ok(()) => filled += 1,
            Err(PushError::Full) => break,
            Err(err) => panic!("unexpected NET_TX fill error: {err:?}"),
        }
    }
    assert!(
        filled > 0,
        "expected NET_TX ring to accept at least one record"
    );

    // Write a second TX descriptor at index 1 and advance TDT.
    let tx_buf2: u64 = 0x24_000;
    let tx_frame2: Vec<u8> = (0..MIN_L2_FRAME_LEN).map(|i| 0x80 | i as u8).collect();
    m.write_physical(tx_buf2, &tx_frame2);
    let mut tx_desc2 = [0u8; 16];
    tx_desc2[0..8].copy_from_slice(&tx_buf2.to_le_bytes());
    tx_desc2[8..10].copy_from_slice(&(tx_frame2.len() as u16).to_le_bytes());
    tx_desc2[11] = 0x01 | 0x08; // EOP | RS
    m.write_physical(tx_desc_base + 16, &tx_desc2);
    m.write_physical_u32(bar0_base + 0x3818, 2); // TDT = 2

    m.poll_network();

    // Drain the dummy frames we used to saturate the ring; the second guest TX frame should not
    // have been pushed because the ring was full.
    for _ in 0..filled {
        assert_eq!(tx_ring.try_pop(), Ok(dummy_frame.clone()));
    }
    assert_eq!(tx_ring.try_pop(), Err(PopError::Empty));

    let stats = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats.tx_pushed_frames, 1);
    assert_eq!(stats.tx_pushed_bytes, tx_frame.len() as u64);
    assert_eq!(stats.tx_dropped_full, 1);
    assert_eq!(stats.tx_dropped_full_bytes, tx_frame2.len() as u64);

    // --------------------------------
    // Host -> guest drop when NET_RX frame is oversize for the ring backend
    // --------------------------------
    rx_ring
        .try_push(&vec![0u8; 3000])
        .expect("NET_RX ring try_push should succeed");
    m.poll_network();

    assert_eq!(rx_ring.try_pop(), Err(PopError::Empty));

    let stats = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats.rx_popped_frames, 1);
    assert_eq!(stats.rx_popped_bytes, rx_frame.len() as u64);
    assert_eq!(stats.rx_dropped_oversize, 1);
    assert_eq!(stats.rx_dropped_oversize_bytes, 3000);

    // Detaching the backend should make ring stats unavailable.
    m.detach_network();
    assert!(m.network_backend_l2_ring_stats().is_none());
}

#[test]
fn machine_virtio_net_l2_tunnel_rings_tx_rx_stats_smoke() {
    let cfg = MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        virtio_net_mac_addr: Some([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
        enable_e1000: false,
        enable_vga: false,
        // Keep the machine minimal and deterministic for this unit test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).expect("Machine::new");

    // No backend attached yet.
    assert!(m.network_backend_l2_ring_stats().is_none());

    // Keep the rings small so tests that intentionally fill them (to validate drop counters)
    // complete quickly.
    let tx_ring = Arc::new(RingBuffer::new(256));
    let rx_ring = Arc::new(RingBuffer::new(4 * 1024));
    m.attach_l2_tunnel_rings(tx_ring.clone(), rx_ring.clone());

    // Initial stats should be present and zeroed.
    assert_eq!(
        m.network_backend_l2_ring_stats(),
        Some(L2TunnelRingBackendStats::default())
    );

    let bdf = aero_devices::pci::profile::VIRTIO_NET.bdf;

    // Enable PCI memory decoding + bus mastering so virtio-net can DMA.
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        0x04,
        2,
        u32::from(command | (1 << 1) | (1 << 2)),
    );

    // Read BAR0 base address via PCI config ports.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-net BAR0 to be assigned");

    // Parse virtio vendor-specific caps to find BAR0 offsets.
    let cfg_bytes = read_config_space_256(&mut m, bdf);
    let caps = parse_caps(&cfg_bytes);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    // Feature negotiation: accept everything the device offers.
    m.write_physical_u8(bar0_base + caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    m.write_physical_u32(bar0_base + caps.common, 0);
    let f0 = m.read_physical_u32(bar0_base + caps.common + 0x04);
    m.write_physical_u32(bar0_base + caps.common + 0x08, 0);
    m.write_physical_u32(bar0_base + caps.common + 0x0c, f0);

    m.write_physical_u32(bar0_base + caps.common, 1);
    let f1 = m.read_physical_u32(bar0_base + caps.common + 0x04);
    m.write_physical_u32(bar0_base + caps.common + 0x08, 1);
    m.write_physical_u32(bar0_base + caps.common + 0x0c, f1);

    m.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    m.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Place virtqueues in RAM above 2MiB so they are not affected by A20 wrap even if A20 is
    // disabled.
    let rx_desc = 0x200000;
    let rx_avail = 0x201000;
    let rx_used = 0x202000;
    let tx_desc = 0x203000;
    let tx_avail = 0x204000;
    let tx_used = 0x205000;

    // Configure RX queue 0.
    m.write_physical_u16(bar0_base + caps.common + 0x16, 0);
    let rx_notify_off = m.read_physical_u16(bar0_base + caps.common + 0x1e);
    assert!(m.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    m.write_physical_u64(bar0_base + caps.common + 0x20, rx_desc);
    m.write_physical_u64(bar0_base + caps.common + 0x28, rx_avail);
    m.write_physical_u64(bar0_base + caps.common + 0x30, rx_used);
    m.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // Configure TX queue 1.
    m.write_physical_u16(bar0_base + caps.common + 0x16, 1);
    let tx_notify_off = m.read_physical_u16(bar0_base + caps.common + 0x1e);
    assert!(m.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    m.write_physical_u64(bar0_base + caps.common + 0x20, tx_desc);
    m.write_physical_u64(bar0_base + caps.common + 0x28, tx_avail);
    m.write_physical_u64(bar0_base + caps.common + 0x30, tx_used);
    m.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    let rx_notify_addr =
        bar0_base + caps.notify + u64::from(rx_notify_off) * u64::from(caps.notify_mult);
    let tx_notify_addr =
        bar0_base + caps.notify + u64::from(tx_notify_off) * u64::from(caps.notify_mult);

    // --------------------------------
    // Guest -> host (TX virtqueue -> NET_TX)
    // --------------------------------
    let hdr_addr = 0x206000;
    let payload_addr = 0x206100;
    let hdr = [0u8; VirtioNetHdr::BASE_LEN];
    let payload = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\x08\x00";
    m.write_physical(hdr_addr, &hdr);
    m.write_physical(payload_addr, payload);

    write_desc(
        &mut m,
        tx_desc,
        0,
        hdr_addr,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(&mut m, tx_desc, 1, payload_addr, payload.len() as u32, 0, 0);

    m.write_physical_u16(tx_avail, 0);
    m.write_physical_u16(tx_avail + 2, 1);
    m.write_physical_u16(tx_avail + 4, 0);
    m.write_physical_u16(tx_used, 0);
    m.write_physical_u16(tx_used + 2, 0);

    // Notify TX queue 1 and pump once. This should DMA the virtqueue chain and push to NET_TX.
    m.write_physical_u16(tx_notify_addr, 1);
    m.poll_network();

    assert_eq!(tx_ring.try_pop(), Ok(payload.to_vec()));
    assert_eq!(tx_ring.try_pop(), Err(PopError::Empty));

    let stats = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats.tx_pushed_frames, 1);
    assert_eq!(stats.tx_pushed_bytes, payload.len() as u64);
    assert_eq!(stats.tx_dropped_oversize, 0);
    assert_eq!(stats.tx_dropped_oversize_bytes, 0);
    assert_eq!(stats.tx_dropped_full, 0);
    assert_eq!(stats.tx_dropped_full_bytes, 0);

    // --------------------------------
    // Host -> guest (NET_RX -> RX virtqueue DMA)
    // --------------------------------
    let rx_hdr_addr = 0x207000;
    let rx_payload_addr = 0x207100;
    m.write_physical(rx_hdr_addr, &[0xaa; VirtioNetHdr::BASE_LEN]);
    m.write_physical(rx_payload_addr, &[0xbb; 64]);

    write_desc(
        &mut m,
        rx_desc,
        0,
        rx_hdr_addr,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        1,
    );
    write_desc(
        &mut m,
        rx_desc,
        1,
        rx_payload_addr,
        64,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    m.write_physical_u16(rx_avail, 0);
    m.write_physical_u16(rx_avail + 2, 1);
    m.write_physical_u16(rx_avail + 4, 0);
    m.write_physical_u16(rx_used, 0);
    m.write_physical_u16(rx_used + 2, 0);

    // Kick RX queue 0 once to register the posted RX buffer, then deliver a host frame.
    m.write_physical_u16(rx_notify_addr, 0);
    m.poll_network();
    assert_eq!(m.read_physical_u16(rx_used + 2), 0);

    let rx_frame = b"\xaa\xbb\xcc\xdd\xee\xff\x00\x01\x02\x03\x04\x05\x08\x00".to_vec();
    rx_ring.try_push(&rx_frame).unwrap();
    m.poll_network();

    assert_eq!(m.read_physical_u16(rx_used + 2), 1);
    assert_eq!(
        m.read_physical_u32(rx_used + 8),
        (VirtioNetHdr::BASE_LEN + rx_frame.len()) as u32
    );

    assert_eq!(
        m.read_physical_bytes(rx_hdr_addr, VirtioNetHdr::BASE_LEN),
        vec![0u8; VirtioNetHdr::BASE_LEN]
    );
    assert_eq!(
        m.read_physical_bytes(rx_payload_addr, rx_frame.len()),
        rx_frame
    );

    assert_eq!(rx_ring.try_pop(), Err(PopError::Empty));

    let stats = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats.rx_popped_frames, 1);
    assert_eq!(stats.rx_popped_bytes, rx_frame.len() as u64);
    assert_eq!(stats.rx_dropped_oversize, 0);
    assert_eq!(stats.rx_dropped_oversize_bytes, 0);
    assert_eq!(stats.rx_corrupt, 0);

    // --------------------------------
    // Guest -> host drop when NET_TX ring is full
    // --------------------------------
    let dummy_frame = vec![0x55u8; 14];
    let mut filled = 0usize;
    loop {
        match tx_ring.try_push(&dummy_frame) {
            Ok(()) => filled += 1,
            Err(PushError::Full) => break,
            Err(err) => panic!("unexpected NET_TX fill error: {err:?}"),
        }
    }
    assert!(
        filled > 0,
        "expected NET_TX ring to accept at least one record"
    );

    // Reuse the existing TX descriptor chain but update the payload and avail.idx.
    let payload2_addr = 0x206200;
    let payload2 = b"\x02\x02\x02\x02\x02\x02\x03\x03\x03\x03\x03\x03\x08\x00";
    m.write_physical(payload2_addr, payload2);
    write_desc(
        &mut m,
        tx_desc,
        1,
        payload2_addr,
        payload2.len() as u32,
        0,
        0,
    );

    // Publish a second avail entry (idx=2, ring[1]=0) and notify queue 1.
    m.write_physical_u16(tx_avail + 2, 2);
    m.write_physical_u16(tx_avail + 4 + 2, 0);
    m.write_physical_u16(tx_notify_addr, 1);
    m.poll_network();

    // Drain the dummy frames used to saturate the ring; the second TX frame should not appear.
    for _ in 0..filled {
        assert_eq!(tx_ring.try_pop(), Ok(dummy_frame.clone()));
    }
    assert_eq!(tx_ring.try_pop(), Err(PopError::Empty));

    let stats = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats.tx_pushed_frames, 1);
    assert_eq!(stats.tx_pushed_bytes, payload.len() as u64);
    assert_eq!(stats.tx_dropped_full, 1);
    assert_eq!(stats.tx_dropped_full_bytes, payload2.len() as u64);

    // --------------------------------
    // Host -> guest drop when NET_RX contains an oversize frame
    // --------------------------------
    // Post another RX buffer (idx=2, ring[1]=0), push an oversize frame into NET_RX, and poll.
    m.write_physical_u16(rx_avail + 2, 2);
    m.write_physical_u16(rx_avail + 4 + 2, 0);
    rx_ring
        .try_push(&vec![0u8; 3000])
        .expect("NET_RX ring try_push should succeed");
    m.write_physical_u16(rx_notify_addr, 0);
    m.poll_network();

    assert_eq!(rx_ring.try_pop(), Err(PopError::Empty));

    let stats = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats.rx_popped_frames, 1);
    assert_eq!(stats.rx_popped_bytes, rx_frame.len() as u64);
    assert_eq!(stats.rx_dropped_oversize, 1);
    assert_eq!(stats.rx_dropped_oversize_bytes, 3000);

    // Detaching the backend should make ring stats unavailable.
    m.detach_network();
    assert!(m.network_backend_l2_ring_stats().is_none());
}
