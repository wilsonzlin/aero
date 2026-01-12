#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_wasm::{HdaPcmWriter, SineTone};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn sine_tone_write_clamps_frames_to_ring_free_space() {
    let bridge = WorkletBridge::new(8, 2).unwrap();
    let mut tone = SineTone::new();

    // A hostile caller could pass a huge frame count; the API should clamp to the ring's free
    // space rather than allocating a massive scratch buffer.
    let written = tone.write(&bridge, u32::MAX, 440.0, 48_000.0, 0.1);
    assert_eq!(written, 8);

    // Now the ring is full; further writes should be dropped without incrementing the ring's
    // overrun counter.
    let written2 = tone.write(&bridge, u32::MAX, 440.0, 48_000.0, 0.1);
    assert_eq!(written2, 0);
    assert_eq!(bridge.overrun_count(), 0);
}

#[wasm_bindgen_test]
fn hda_pcm_writer_clamps_dst_sample_rate_to_avoid_oom() {
    let writer = HdaPcmWriter::new(u32::MAX).unwrap();
    assert_eq!(writer.dst_sample_rate_hz(), aero_audio::MAX_HOST_SAMPLE_RATE_HZ);

    let mut writer2 = HdaPcmWriter::new(48_000).unwrap();
    writer2.set_dst_sample_rate_hz(u32::MAX).unwrap();
    assert_eq!(writer2.dst_sample_rate_hz(), aero_audio::MAX_HOST_SAMPLE_RATE_HZ);
}

#[wasm_bindgen_test]
fn hda_pcm_writer_does_not_buffer_when_ring_is_full() {
    let bridge = WorkletBridge::new(4, 2).unwrap();
    // Fill the ring so there is no free space.
    assert_eq!(bridge.write_f32_interleaved(&[0.0f32; 8]), 4);
    assert_eq!(bridge.buffer_level_frames(), 4);

    let mut writer = HdaPcmWriter::new(48_000).unwrap();

    // Provide some PCM bytes; since the ring is full, the writer should return early without
    // attempting to decode/resample or pushing dropped frames into the ring's overrun counter.
    let pcm = vec![0u8; 4 * 4]; // 4 frames of 16-bit stereo (4 bytes/frame)
    let wrote = writer.push_hda_pcm_bytes(&bridge, 0x0011, &pcm).unwrap();
    assert_eq!(wrote, 0);
    assert_eq!(bridge.overrun_count(), 0);
}

