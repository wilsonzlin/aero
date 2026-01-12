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
