use std::path::PathBuf;

use aero_platform::audio::worklet_bridge::{
    frames_available, frames_available_clamped, frames_free,
};
use serde::Deserialize;

/// Shared conformance vectors for AudioWorklet playback ring index math.
///
/// These vectors are consumed by both Rust (`aero-platform`) and the web unit tests to prevent the
/// AudioWorklet ring math implementations from drifting apart.
///
/// If you intentionally change the semantics, update:
/// - `crates/platform/src/audio/worklet_bridge.rs`
/// - `web/src/platform/audio_worklet_ring_layout.js`
/// - `tests/fixtures/audio_worklet_ring_vectors.json`
#[derive(Debug, Deserialize)]
struct Vector {
    #[serde(default)]
    name: Option<String>,
    read_idx: u32,
    write_idx: u32,
    capacity_frames: u32,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Expected {
    frames_available: u32,
    frames_available_clamped: u32,
    frames_free: u32,
}

#[test]
fn audio_worklet_ring_math_matches_shared_vectors() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/audio_worklet_ring_vectors.json");
    let fixture_bytes = std::fs::read(&fixture_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", fixture_path.display()));

    let vectors: Vec<Vector> = serde_json::from_slice(&fixture_bytes)
        .unwrap_or_else(|err| panic!("deserialize {}: {err}", fixture_path.display()));
    assert!(
        !vectors.is_empty(),
        "fixture must contain at least one vector"
    );

    for (i, v) in vectors.iter().enumerate() {
        let label = v
            .name
            .as_deref()
            .unwrap_or("<unnamed vector; please add a name>");

        let available = frames_available(v.read_idx, v.write_idx);
        assert_eq!(
            available, v.expected.frames_available,
            "vector[{i}] {label}: frames_available(read_idx={}, write_idx={}, capacity_frames={})",
            v.read_idx, v.write_idx, v.capacity_frames
        );

        let clamped = frames_available_clamped(v.read_idx, v.write_idx, v.capacity_frames);
        assert_eq!(
            clamped, v.expected.frames_available_clamped,
            "vector[{i}] {label}: frames_available_clamped(read_idx={}, write_idx={}, capacity_frames={})",
            v.read_idx, v.write_idx, v.capacity_frames
        );

        let free = frames_free(v.read_idx, v.write_idx, v.capacity_frames);
        assert_eq!(
            free, v.expected.frames_free,
            "vector[{i}] {label}: frames_free(read_idx={}, write_idx={}, capacity_frames={})",
            v.read_idx, v.write_idx, v.capacity_frames
        );
    }
}
