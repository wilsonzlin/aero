#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::{
    WorkletBridge, HEADER_BYTES, HEADER_U32_LEN, OVERRUN_COUNT_INDEX, READ_FRAME_INDEX,
    WRITE_FRAME_INDEX,
};
use js_sys::{Float32Array, Uint32Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Atomics, js_name = load)]
    fn atomics_load_u32(array: &Uint32Array, index: u32) -> u32;

    #[wasm_bindgen(js_namespace = Atomics, js_name = store)]
    fn atomics_store_u32(array: &Uint32Array, index: u32, value: u32) -> u32;
}

#[wasm_bindgen_test]
fn worklet_bridge_counts_dropped_frames_and_advances_write_index() {
    let capacity_frames = 4;
    let channel_count = 2;
    let bridge = WorkletBridge::new(capacity_frames, channel_count).unwrap();

    let sab = bridge.shared_buffer();
    let header = Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
    let ring_samples = Float32Array::new_with_byte_offset_and_length(
        &sab,
        HEADER_BYTES as u32,
        capacity_frames * channel_count,
    );

    // Request more frames than the ring buffer can fit.
    let input: Vec<f32> = (0..(6 * channel_count)).map(|v| v as f32).collect();
    let written = bridge.write_f32_interleaved(&input);
    assert_eq!(written, capacity_frames);

    // Overrun is measured in dropped frames.
    assert_eq!(bridge.overrun_count(), 2);
    assert_eq!(atomics_load_u32(&header, OVERRUN_COUNT_INDEX as u32), 2);

    // The producer index should advance only by the frames actually written.
    assert_eq!(atomics_load_u32(&header, WRITE_FRAME_INDEX as u32), 4);

    // Verify the first chunk of samples was copied into the ring.
    for i in 0..(capacity_frames * channel_count) {
        assert_eq!(ring_samples.get_index(i), i as f32);
    }

    // A fully-dropped write should still increment the overrun counter.
    let input2 = vec![123.0; 2 * channel_count as usize];
    let written2 = bridge.write_f32_interleaved(&input2);
    assert_eq!(written2, 0);
    assert_eq!(bridge.overrun_count(), 4);
    assert_eq!(atomics_load_u32(&header, OVERRUN_COUNT_INDEX as u32), 4);
    assert_eq!(atomics_load_u32(&header, WRITE_FRAME_INDEX as u32), 4);
}

#[wasm_bindgen_test]
fn worklet_bridge_bulk_copy_handles_wraparound() {
    let capacity_frames = 4;
    let channel_count = 2;
    let bridge = WorkletBridge::new(capacity_frames, channel_count).unwrap();

    let sab = bridge.shared_buffer();
    let header = Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
    let ring_samples = Float32Array::new_with_byte_offset_and_length(
        &sab,
        HEADER_BYTES as u32,
        capacity_frames * channel_count,
    );

    // Write 3 frames -> write_pos ends at frame 3 (wrap point).
    let first: Vec<f32> = (0..(3 * channel_count)).map(|v| v as f32).collect();
    assert_eq!(bridge.write_f32_interleaved(&first), 3);
    assert_eq!(atomics_load_u32(&header, WRITE_FRAME_INDEX as u32), 3);

    // Simulate the consumer draining 2 frames.
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 2);

    // This write should wrap: 1 frame at the end, 2 at the start.
    let second: Vec<f32> = (100..(100 + 3 * channel_count)).map(|v| v as f32).collect();
    assert_eq!(bridge.write_f32_interleaved(&second), 3);
    assert_eq!(bridge.overrun_count(), 0);
    assert_eq!(atomics_load_u32(&header, WRITE_FRAME_INDEX as u32), 6);

    // Expected ring layout after wrap:
    // - frame 0: [102, 103]
    // - frame 1: [104, 105]
    // - frame 2: [4, 5] (unread from the first write)
    // - frame 3: [100, 101]
    let expected: [f32; 8] = [102.0, 103.0, 104.0, 105.0, 4.0, 5.0, 100.0, 101.0];
    for (i, value) in expected.iter().enumerate() {
        assert_eq!(ring_samples.get_index(i as u32), *value);
    }
}
