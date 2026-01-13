use aero_audio::ring::AudioRingBuffer;

fn stereo_frames(values: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(values.len() * 2);
    for &v in values {
        out.push(v);
        out.push(v);
    }
    out
}

#[test]
fn audio_ring_drops_new_frames_when_full() {
    let mut ring = AudioRingBuffer::new_stereo(4);

    ring.push_interleaved_stereo(&stereo_frames(&[0.0, 1.0, 2.0, 3.0]));
    assert_eq!(ring.available_frames(), 4);

    // Buffer is full; the new frames should be dropped (not overwrite existing data).
    ring.push_interleaved_stereo(&stereo_frames(&[4.0, 5.0]));
    assert_eq!(ring.telemetry().overrun_frames, 2);
    assert_eq!(ring.available_frames(), 4);

    let out = ring.pop_interleaved_stereo(4);
    assert_eq!(out, stereo_frames(&[0.0, 1.0, 2.0, 3.0]));
}

#[test]
fn audio_ring_truncates_writes_and_preserves_order() {
    let mut ring = AudioRingBuffer::new_stereo(4);

    ring.push_interleaved_stereo(&stereo_frames(&[0.0, 1.0, 2.0]));
    ring.push_interleaved_stereo(&stereo_frames(&[3.0, 4.0, 5.0]));

    // Only one frame was free, so two frames are dropped.
    assert_eq!(ring.telemetry().overrun_frames, 2);
    assert_eq!(ring.available_frames(), 4);

    let out = ring.pop_interleaved_stereo(4);
    assert_eq!(out, stereo_frames(&[0.0, 1.0, 2.0, 3.0]));
}

#[test]
fn audio_ring_does_not_overwrite_existing_frames_across_wraparound() {
    let mut ring = AudioRingBuffer::new_stereo(4);

    ring.push_interleaved_stereo(&stereo_frames(&[0.0, 1.0, 2.0, 3.0]));
    assert_eq!(ring.pop_interleaved_stereo(2), stereo_frames(&[0.0, 1.0]));

    // The write cursor has wrapped to the start; ensure we still preserve frames 2 and 3.
    ring.push_interleaved_stereo(&stereo_frames(&[4.0, 5.0, 6.0]));
    assert_eq!(ring.telemetry().overrun_frames, 1);

    let out = ring.pop_interleaved_stereo(4);
    assert_eq!(out, stereo_frames(&[2.0, 3.0, 4.0, 5.0]));
}

#[test]
fn audio_ring_clamps_excessive_capacity_to_avoid_oom() {
    // Keep this in sync with the internal cap in `aero_audio::ring`.
    const MAX_CAPACITY_FRAMES: usize = 1_048_576;

    let ring = AudioRingBuffer::new_stereo(usize::MAX);
    assert_eq!(ring.capacity_frames(), MAX_CAPACITY_FRAMES);
}

#[test]
fn audio_ring_clamps_zero_capacity_to_avoid_panics() {
    let ring = AudioRingBuffer::new_stereo(0);
    assert_eq!(ring.capacity_frames(), 1);
}

#[test]
fn audio_ring_push_ignores_trailing_partial_frame() {
    let mut ring = AudioRingBuffer::new_stereo(4);
    // Odd number of samples: should ignore the trailing value instead of panicking.
    ring.push_interleaved_stereo(&[1.0, 2.0, 3.0]);
    assert_eq!(ring.available_frames(), 1);
    assert_eq!(ring.pop_interleaved_stereo(1), vec![1.0, 2.0]);
}

#[test]
fn audio_ring_pop_clamps_absurd_frames_to_avoid_oom() {
    // Keep this in sync with the internal cap in `aero_audio::ring`.
    const MAX_CAPACITY_FRAMES: usize = 1_048_576;

    let mut ring = AudioRingBuffer::new_stereo(4);
    let out = ring.pop_interleaved_stereo(usize::MAX);
    assert_eq!(out.len(), MAX_CAPACITY_FRAMES * 2);
    assert_eq!(ring.telemetry().underrun_frames, MAX_CAPACITY_FRAMES as u64);
}

#[test]
fn audio_ring_wraparound_bulk_copy_preserves_order() {
    // Exercise a push that wraps within a *single call* (two-segment copy) and a
    // pop that wraps within a single call, asserting that frame order is
    // preserved end-to-end.
    let mut ring = AudioRingBuffer::new_stereo(4);

    ring.push_interleaved_stereo(&stereo_frames(&[0.0, 1.0, 2.0]));
    assert_eq!(ring.pop_interleaved_stereo(2), stereo_frames(&[0.0, 1.0]));

    // The write cursor is near the end; writing 3 frames must wrap and place the
    // final two frames at the start of the buffer.
    ring.push_interleaved_stereo(&stereo_frames(&[3.0, 4.0, 5.0]));
    assert_eq!(ring.telemetry().overrun_frames, 0);
    assert_eq!(ring.available_frames(), 4);

    // Reading should observe the remaining frame from the first push, then the
    // wrapped frames from the second push.
    let out = ring.pop_interleaved_stereo(4);
    assert_eq!(out, stereo_frames(&[2.0, 3.0, 4.0, 5.0]));
}

#[test]
fn audio_ring_pop_into_zero_fills_underrun_tail() {
    let mut ring = AudioRingBuffer::new_stereo(4);
    ring.push_interleaved_stereo(&stereo_frames(&[1.0, 2.0]));

    // Pre-fill the output buffer with non-zero data to ensure the underrun path
    // overwrites/clears the tail even when reusing a `Vec`.
    let mut out = vec![42.0f32; 4 * 2];
    ring.pop_interleaved_stereo_into(4, &mut out);

    assert_eq!(out, stereo_frames(&[1.0, 2.0, 0.0, 0.0]));
    assert_eq!(ring.available_frames(), 0);
    assert_eq!(ring.telemetry().underrun_frames, 2);
}
