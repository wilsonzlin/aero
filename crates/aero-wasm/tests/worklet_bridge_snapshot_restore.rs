#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::{
    HEADER_BYTES, HEADER_U32_LEN, READ_FRAME_INDEX, WRITE_FRAME_INDEX, WorkletBridge,
};
use aero_wasm::HdaControllerBridge;
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
fn worklet_bridge_snapshot_restore_restores_indices_and_clears_samples() {
    let capacity_frames = 8;
    let channel_count = 2;
    let bridge = WorkletBridge::new(capacity_frames, channel_count).unwrap();

    let sab = bridge.shared_buffer();
    let header = Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
    let samples = Float32Array::new_with_byte_offset_and_length(
        &sab,
        HEADER_BYTES as u32,
        capacity_frames * channel_count,
    );

    // Seed the ring with non-zero audio and a non-trivial read/write position.
    let input: Vec<f32> = (0..(6 * channel_count)).map(|v| (v + 1) as f32).collect();
    assert_eq!(bridge.write_f32_interleaved(&input), 6);
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 2);

    // Snapshot the ring indices.
    let snap = bridge.snapshot_state();
    assert_eq!(snap.capacity_frames, capacity_frames);
    assert_eq!(snap.read_pos, 2);
    assert_eq!(snap.write_pos, 6);

    // Corrupt both indices and samples.
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 123);
    atomics_store_u32(&header, WRITE_FRAME_INDEX as u32, 456);
    let _ = samples.fill(123.0, 0, samples.length());

    // Restore and verify.
    bridge.restore_state(&snap);
    assert_eq!(
        atomics_load_u32(&header, READ_FRAME_INDEX as u32),
        snap.read_pos
    );
    assert_eq!(
        atomics_load_u32(&header, WRITE_FRAME_INDEX as u32),
        snap.write_pos
    );

    for i in 0..samples.length() {
        assert_eq!(samples.get_index(i), 0.0, "sample[{i}] not cleared");
    }
}

#[wasm_bindgen_test]
fn hda_controller_save_load_restores_worklet_ring_and_clears_samples() {
    let capacity_frames = 8;
    let channel_count = 2;

    // Back the guest memory mapping with a simple heap allocation.
    let mut guest = vec![0u8; 0x4000];
    let guest_base = guest.as_mut_ptr() as u32;
    let guest_size = guest.len() as u32;

    let mut hda = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();

    let ring = WorkletBridge::new(capacity_frames, channel_count).unwrap();
    let sab = ring.shared_buffer();
    hda.attach_audio_ring(sab.clone(), capacity_frames, channel_count)
        .unwrap();

    let header = Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
    let samples = Float32Array::new_with_byte_offset_and_length(
        &sab,
        HEADER_BYTES as u32,
        capacity_frames * channel_count,
    );

    // Seed ring state.
    let input: Vec<f32> = (0..(6 * channel_count)).map(|v| (v + 1) as f32).collect();
    assert_eq!(ring.write_f32_interleaved(&input), 6);
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 2);

    let snap = hda.save_state();

    // Corrupt both indices and samples.
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 123);
    atomics_store_u32(&header, WRITE_FRAME_INDEX as u32, 456);
    let _ = samples.fill(123.0, 0, samples.length());

    // Restore via the HDA bridge snapshot path.
    hda.load_state(&snap).unwrap();

    assert_eq!(atomics_load_u32(&header, READ_FRAME_INDEX as u32), 2);
    assert_eq!(atomics_load_u32(&header, WRITE_FRAME_INDEX as u32), 6);
    for i in 0..samples.length() {
        assert_eq!(samples.get_index(i), 0.0, "sample[{i}] not cleared");
    }

    drop(guest);
}

#[wasm_bindgen_test]
fn hda_controller_deferred_ring_restore_applies_on_attach() {
    let capacity_frames = 8;
    let channel_count = 2;

    let mut guest = vec![0u8; 0x4000];
    let guest_base = guest.as_mut_ptr() as u32;
    let guest_size = guest.len() as u32;

    let mut hda = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();

    let ring = WorkletBridge::new(capacity_frames, channel_count).unwrap();
    let sab = ring.shared_buffer();
    hda.attach_audio_ring(sab.clone(), capacity_frames, channel_count)
        .unwrap();

    let header = Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
    let samples = Float32Array::new_with_byte_offset_and_length(
        &sab,
        HEADER_BYTES as u32,
        capacity_frames * channel_count,
    );

    let input: Vec<f32> = (0..(6 * channel_count)).map(|v| (v + 1) as f32).collect();
    assert_eq!(ring.write_f32_interleaved(&input), 6);
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 2);
    let snap = hda.save_state();

    // Simulate a host runtime that tears down and later recreates the AudioWorklet ring after restore.
    hda.detach_audio_ring();

    // Corrupt indices and samples.
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 123);
    atomics_store_u32(&header, WRITE_FRAME_INDEX as u32, 456);
    let _ = samples.fill(123.0, 0, samples.length());

    // Restore snapshot bytes while ring is detached; this should defer ring restore.
    hda.load_state(&snap).unwrap();
    assert_eq!(atomics_load_u32(&header, READ_FRAME_INDEX as u32), 123);

    // Reattach the ring; the deferred state should now be applied and samples cleared.
    hda.attach_audio_ring(sab.clone(), capacity_frames, channel_count)
        .unwrap();
    assert_eq!(atomics_load_u32(&header, READ_FRAME_INDEX as u32), 2);
    assert_eq!(atomics_load_u32(&header, WRITE_FRAME_INDEX as u32), 6);
    for i in 0..samples.length() {
        assert_eq!(samples.get_index(i), 0.0, "sample[{i}] not cleared");
    }

    drop(guest);
}
