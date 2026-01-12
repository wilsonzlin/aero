#![cfg(feature = "io-snapshot")]

use aero_net_e1000::{E1000Device, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};

fn build_frame(id: u16) -> Vec<u8> {
    let mut frame = Vec::with_capacity(MIN_L2_FRAME_LEN + 2);
    // Ethernet header (dst/src/ethertype).
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(&id.to_le_bytes());
    frame
}

fn frame_id(frame: &[u8]) -> u16 {
    assert!(
        frame.len() >= MIN_L2_FRAME_LEN + 2,
        "frame too short to contain id payload"
    );
    u16::from_le_bytes([frame[MIN_L2_FRAME_LEN], frame[MIN_L2_FRAME_LEN + 1]])
}

#[test]
fn restore_state_clamps_host_queues_and_filters_invalid_frames() {
    let dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    let mut state = dev.snapshot_state();

    let invalid_short = vec![0u8; MIN_L2_FRAME_LEN - 1];
    let invalid_long = vec![0u8; MAX_L2_FRAME_LEN + 1];

    let mut frames = Vec::new();
    frames.push(invalid_short.clone());
    frames.push(invalid_long.clone());
    for id in 0u16..300 {
        frames.push(build_frame(id));
    }

    // Populate both host-facing queues with a mix of invalid + oversized counts so restore logic
    // must filter lengths and clamp to the configured maximum (256 entries).
    state.rx_pending = frames.clone();
    state.tx_out = frames;

    let mut restored = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    restored.restore_state(&state);

    let restored_state = restored.snapshot_state();
    assert_eq!(
        restored_state.rx_pending.len(),
        256,
        "rx_pending should be clamped"
    );
    assert_eq!(restored_state.tx_out.len(), 256, "tx_out should be clamped");

    let rx_ids: Vec<u16> = restored_state
        .rx_pending
        .iter()
        .map(|f| frame_id(f))
        .collect();
    let tx_ids: Vec<u16> = restored_state.tx_out.iter().map(|f| frame_id(f)).collect();

    // After filtering invalid frames, we have IDs 0..=299. Clamping to 256 should preserve the most
    // recent 256 entries: 44..=299.
    let expected: Vec<u16> = (44u16..300).collect();
    assert_eq!(rx_ids, expected);
    assert_eq!(tx_ids, expected);
}
