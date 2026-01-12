#![cfg(target_arch = "wasm32")]

use aero_platform::audio::worklet_bridge::{
    WorkletBridge, HEADER_BYTES, HEADER_U32_LEN, OVERRUN_COUNT_INDEX, READ_FRAME_INDEX,
    UNDERRUN_COUNT_INDEX, WRITE_FRAME_INDEX,
};
use aero_platform::audio::mic_bridge as mic_ring;
use aero_wasm::{attach_mic_bridge, HdaControllerBridge, VirtioSndPciBridge};
use js_sys::{Float32Array, SharedArrayBuffer, Uint32Array};
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
fn worklet_bridge_restore_does_not_modify_underrun_overrun_counters() {
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

    // Seed indices + counters.
    let input: Vec<f32> = (0..(4 * channel_count)).map(|v| (v + 1) as f32).collect();
    assert_eq!(bridge.write_f32_interleaved(&input), 4);
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 1);
    atomics_store_u32(&header, UNDERRUN_COUNT_INDEX as u32, 123);
    atomics_store_u32(&header, OVERRUN_COUNT_INDEX as u32, 456);

    let snap = bridge.snapshot_state();

    // Corrupt indices, counters, and samples.
    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 111);
    atomics_store_u32(&header, WRITE_FRAME_INDEX as u32, 222);
    atomics_store_u32(&header, UNDERRUN_COUNT_INDEX as u32, 777);
    atomics_store_u32(&header, OVERRUN_COUNT_INDEX as u32, 888);
    let _ = samples.fill(123.0, 0, samples.length());

    // Restore indices + samples; counters should remain untouched.
    bridge.restore_state(&snap);

    assert_eq!(
        atomics_load_u32(&header, READ_FRAME_INDEX as u32),
        snap.read_pos
    );
    assert_eq!(
        atomics_load_u32(&header, WRITE_FRAME_INDEX as u32),
        snap.write_pos
    );
    assert_eq!(atomics_load_u32(&header, UNDERRUN_COUNT_INDEX as u32), 777);
    assert_eq!(atomics_load_u32(&header, OVERRUN_COUNT_INDEX as u32), 888);
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

#[wasm_bindgen_test]
fn hda_controller_deferred_ring_restore_tolerates_capacity_mismatch() {
    let snapshot_capacity_frames = 8;
    let restored_capacity_frames = 4;
    let channel_count = 2;

    let mut guest = vec![0u8; 0x4000];
    let guest_base = guest.as_mut_ptr() as u32;
    let guest_size = guest.len() as u32;

    let mut hda = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();

    // Create a snapshot with a larger AudioWorklet ring.
    let ring = WorkletBridge::new(snapshot_capacity_frames, channel_count).unwrap();
    let sab = ring.shared_buffer();
    hda.attach_audio_ring(sab, snapshot_capacity_frames, channel_count)
        .unwrap();

    // Fill the ring so the write index is far ahead of the read index (used > restored capacity).
    let input: Vec<f32> = (0..(snapshot_capacity_frames * channel_count))
        .map(|v| (v + 1) as f32)
        .collect();
    assert_eq!(ring.write_f32_interleaved(&input), snapshot_capacity_frames);
    let snap = hda.save_state();

    // Detach the ring and restore the snapshot bytes while no ring is attached. This will stash the
    // ring state for later application.
    hda.detach_audio_ring();
    hda.load_state(&snap).unwrap();

    // Reattach a smaller ring. Applying the pending snapshot must not panic, and indices should be
    // clamped to the new capacity.
    let ring2 = WorkletBridge::new(restored_capacity_frames, channel_count).unwrap();
    let sab2 = ring2.shared_buffer();
    let header = Uint32Array::new_with_byte_offset_and_length(&sab2, 0, HEADER_U32_LEN as u32);
    let samples = Float32Array::new_with_byte_offset_and_length(
        &sab2,
        HEADER_BYTES as u32,
        restored_capacity_frames * channel_count,
    );

    atomics_store_u32(&header, READ_FRAME_INDEX as u32, 123);
    atomics_store_u32(&header, WRITE_FRAME_INDEX as u32, 456);
    let _ = samples.fill(123.0, 0, samples.length());

    hda.attach_audio_ring(sab2, restored_capacity_frames, channel_count)
        .unwrap();

    // The snapshot had read=0, write=8. Restoring into capacity 4 should clamp read to write-4.
    assert_eq!(atomics_load_u32(&header, READ_FRAME_INDEX as u32), 4);
    assert_eq!(atomics_load_u32(&header, WRITE_FRAME_INDEX as u32), 8);
    for i in 0..samples.length() {
        assert_eq!(samples.get_index(i), 0.0, "sample[{i}] not cleared");
    }

    drop(guest);
}

#[wasm_bindgen_test]
fn hda_controller_attach_mic_ring_discards_buffered_samples() {
    let capacity_samples = 16u32;
    let byte_len = (mic_ring::HEADER_BYTES + capacity_samples as usize * core::mem::size_of::<f32>())
        as u32;
    let sab = SharedArrayBuffer::new(byte_len);
    let header =
        Uint32Array::new_with_byte_offset_and_length(&sab, 0, mic_ring::HEADER_U32_LEN as u32);

    // Seed with a non-empty ring state.
    atomics_store_u32(&header, mic_ring::WRITE_POS_INDEX as u32, 10);
    atomics_store_u32(&header, mic_ring::READ_POS_INDEX as u32, 4);
    atomics_store_u32(&header, mic_ring::DROPPED_SAMPLES_INDEX as u32, 0);
    atomics_store_u32(&header, mic_ring::CAPACITY_SAMPLES_INDEX as u32, capacity_samples);

    let mut guest = vec![0u8; 0x4000];
    let guest_base = guest.as_mut_ptr() as u32;
    let guest_size = guest.len() as u32;
    let mut hda = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();

    hda.attach_mic_ring(sab.clone(), 48_000).unwrap();

    // Attaching should drop any buffered samples so capture starts at low latency.
    assert_eq!(
        atomics_load_u32(&header, mic_ring::READ_POS_INDEX as u32),
        atomics_load_u32(&header, mic_ring::WRITE_POS_INDEX as u32)
    );

    drop(guest);
}

#[wasm_bindgen_test]
fn hda_controller_load_state_discards_mic_ring_buffered_samples() {
    let capacity_samples = 16u32;
    let byte_len = (mic_ring::HEADER_BYTES + capacity_samples as usize * core::mem::size_of::<f32>())
        as u32;
    let sab = SharedArrayBuffer::new(byte_len);
    let header =
        Uint32Array::new_with_byte_offset_and_length(&sab, 0, mic_ring::HEADER_U32_LEN as u32);

    // Initialize the header (ring is empty at attach time).
    atomics_store_u32(&header, mic_ring::WRITE_POS_INDEX as u32, 0);
    atomics_store_u32(&header, mic_ring::READ_POS_INDEX as u32, 0);
    atomics_store_u32(&header, mic_ring::DROPPED_SAMPLES_INDEX as u32, 0);
    atomics_store_u32(&header, mic_ring::CAPACITY_SAMPLES_INDEX as u32, capacity_samples);

    let mut guest = vec![0u8; 0x4000];
    let guest_base = guest.as_mut_ptr() as u32;
    let guest_size = guest.len() as u32;
    let mut hda = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();

    hda.attach_mic_ring(sab.clone(), 48_000).unwrap();

    let snap = hda.save_state();

    // Simulate the producer writing into the ring while the VM is snapshot-paused.
    atomics_store_u32(&header, mic_ring::WRITE_POS_INDEX as u32, 10);
    atomics_store_u32(&header, mic_ring::READ_POS_INDEX as u32, 4);

    assert_ne!(
        atomics_load_u32(&header, mic_ring::READ_POS_INDEX as u32),
        atomics_load_u32(&header, mic_ring::WRITE_POS_INDEX as u32)
    );

    // Snapshot restore should discard any buffered mic samples so capture resumes without stale
    // latency.
    hda.load_state(&snap).unwrap();
    assert_eq!(
        atomics_load_u32(&header, mic_ring::READ_POS_INDEX as u32),
        atomics_load_u32(&header, mic_ring::WRITE_POS_INDEX as u32)
    );

    drop(guest);
}

#[wasm_bindgen_test]
fn attach_mic_bridge_discards_buffered_samples() {
    let capacity_samples = 16u32;
    let byte_len = (mic_ring::HEADER_BYTES + capacity_samples as usize * core::mem::size_of::<f32>())
        as u32;
    let sab = SharedArrayBuffer::new(byte_len);
    let header =
        Uint32Array::new_with_byte_offset_and_length(&sab, 0, mic_ring::HEADER_U32_LEN as u32);

    // Seed with a non-empty ring state.
    atomics_store_u32(&header, mic_ring::WRITE_POS_INDEX as u32, 10);
    atomics_store_u32(&header, mic_ring::READ_POS_INDEX as u32, 4);
    atomics_store_u32(&header, mic_ring::DROPPED_SAMPLES_INDEX as u32, 0);
    atomics_store_u32(&header, mic_ring::CAPACITY_SAMPLES_INDEX as u32, capacity_samples);

    let bridge = attach_mic_bridge(sab.clone()).unwrap();

    assert_eq!(
        atomics_load_u32(&header, mic_ring::READ_POS_INDEX as u32),
        atomics_load_u32(&header, mic_ring::WRITE_POS_INDEX as u32)
    );
    assert_eq!(bridge.buffered_samples(), 0);
}

#[wasm_bindgen_test]
fn virtio_snd_pci_bridge_attach_mic_ring_discards_buffered_samples() {
    let capacity_samples = 16u32;
    let byte_len = (mic_ring::HEADER_BYTES + capacity_samples as usize * core::mem::size_of::<f32>())
        as u32;
    let sab = SharedArrayBuffer::new(byte_len);
    let header =
        Uint32Array::new_with_byte_offset_and_length(&sab, 0, mic_ring::HEADER_U32_LEN as u32);

    // Seed with a non-empty ring state.
    atomics_store_u32(&header, mic_ring::WRITE_POS_INDEX as u32, 10);
    atomics_store_u32(&header, mic_ring::READ_POS_INDEX as u32, 4);
    atomics_store_u32(&header, mic_ring::DROPPED_SAMPLES_INDEX as u32, 0);
    atomics_store_u32(&header, mic_ring::CAPACITY_SAMPLES_INDEX as u32, capacity_samples);

    let mut guest = vec![0u8; 0x4000];
    let guest_base = guest.as_mut_ptr() as u32;
    let guest_size = guest.len() as u32;
    let mut snd = VirtioSndPciBridge::new(guest_base, guest_size).unwrap();

    snd.set_mic_ring_buffer(Some(sab.clone())).unwrap();

    assert_eq!(
        atomics_load_u32(&header, mic_ring::READ_POS_INDEX as u32),
        atomics_load_u32(&header, mic_ring::WRITE_POS_INDEX as u32)
    );

    drop(guest);
}
