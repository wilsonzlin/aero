# Mic ring index math vectors

This fixture (`mic_ring_vectors.json`) contains shared conformance vectors for the microphone
capture ring buffer index math.

The same semantics are implemented in two places:

- Rust: `crates/platform/src/audio/mic_bridge.rs`
- JS: `web/src/audio/mic_ring.js`

The fields in each test case are `u32` sample counters (wrapping at `2^32`):

- `samples_available = write_pos - read_pos (wrapping u32)`
- `samples_available_clamped = min(samples_available, capacity_samples)`
- `samples_free = capacity_samples - samples_available_clamped`

If you intentionally change the ring math semantics, update:

1. Rust implementation (`mic_bridge.rs`)
2. JS implementation (`mic_ring.js`)
3. Expected values in `mic_ring_vectors.json`

Both the Rust unit tests and the Web (vitest) unit tests consume this file to prevent subtle drift.
