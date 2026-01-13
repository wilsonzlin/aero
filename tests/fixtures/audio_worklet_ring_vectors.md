# AudioWorklet ring index math vectors

This fixture (`audio_worklet_ring_vectors.json`) contains shared conformance vectors for the
AudioWorklet playback ring buffer index math.

The same semantics are implemented in two places:

- Rust: `crates/platform/src/audio/worklet_bridge.rs`
- JS (AudioWorklet-safe): `web/src/platform/audio_worklet_ring_layout.js`

The fields in each test case are `u32` frame counters (wrapping at `2^32`):

- `frames_available = write_idx - read_idx (wrapping u32)`
- `frames_available_clamped = min(frames_available, capacity_frames)`
- `frames_free = capacity_frames - frames_available_clamped`

If you intentionally change the ring math semantics, update:

1. Rust implementation (`worklet_bridge.rs`)
2. JS implementation (`audio_worklet_ring_layout.js`)
3. Expected values in `audio_worklet_ring_vectors.json`

Both the Rust unit tests and the Web (vitest) unit tests consume this file to prevent subtle drift.

