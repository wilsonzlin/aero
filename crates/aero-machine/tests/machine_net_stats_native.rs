#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_ipc::ring::RingBuffer;
use aero_machine::{Machine, MachineConfig};
use aero_net_e1000::MIN_L2_FRAME_LEN;

#[test]
fn machine_network_backend_l2_ring_stats_reports_ring_backend_stats() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_e1000: true,
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

    let tx = Arc::new(RingBuffer::new(64));
    let rx = Arc::new(RingBuffer::new(64));
    m.attach_l2_tunnel_rings(tx, rx.clone());

    // Initial stats should be present and zeroed.
    let stats0 = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats0.rx_popped_frames, 0);
    assert_eq!(stats0.rx_dropped_oversize, 0);
    assert_eq!(stats0.rx_corrupt, 0);

    // Push one host->guest frame into NET_RX and run the network pump once. The ring backend
    // should pop it (even if the guest hasn't enabled RX DMA yet), incrementing the RX counters.
    let frame = vec![0u8; MIN_L2_FRAME_LEN];
    rx.try_push(&frame).expect("rx.try_push");
    m.poll_network();

    let stats1 = m
        .network_backend_l2_ring_stats()
        .expect("expected ring backend stats");
    assert_eq!(stats1.rx_popped_frames, 1);

    // Detaching should stop reporting stats.
    m.detach_network();
    assert!(m.network_backend_l2_ring_stats().is_none());
}
