use std::path::PathBuf;

use aero_platform::audio::mic_bridge::{
    samples_available, samples_available_clamped, samples_free,
};
use serde::Deserialize;

/// Shared conformance vectors for microphone capture ring index math.
///
/// These vectors are consumed by both Rust (`aero-platform`) and the web unit tests to prevent the
/// mic ring math implementations from drifting apart.
///
/// If you intentionally change the semantics, update:
/// - `crates/platform/src/audio/mic_bridge.rs`
/// - `web/src/audio/mic_ring.js`
/// - `tests/fixtures/mic_ring_vectors.json`
#[derive(Debug, Deserialize)]
struct Vector {
    #[serde(default)]
    name: Option<String>,
    read_pos: u32,
    write_pos: u32,
    capacity_samples: u32,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Expected {
    samples_available: u32,
    samples_available_clamped: u32,
    samples_free: u32,
}

#[test]
fn mic_ring_math_matches_shared_vectors() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/mic_ring_vectors.json");
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

        let available = samples_available(v.read_pos, v.write_pos);
        assert_eq!(
            available, v.expected.samples_available,
            "vector[{i}] {label}: samples_available(read_pos={}, write_pos={}, capacity_samples={})",
            v.read_pos, v.write_pos, v.capacity_samples
        );

        let clamped = samples_available_clamped(v.read_pos, v.write_pos, v.capacity_samples);
        assert_eq!(
            clamped, v.expected.samples_available_clamped,
            "vector[{i}] {label}: samples_available_clamped(read_pos={}, write_pos={}, capacity_samples={})",
            v.read_pos, v.write_pos, v.capacity_samples
        );

        let free = samples_free(v.read_pos, v.write_pos, v.capacity_samples);
        assert_eq!(
            free, v.expected.samples_free,
            "vector[{i}] {label}: samples_free(read_pos={}, write_pos={}, capacity_samples={})",
            v.read_pos, v.write_pos, v.capacity_samples
        );
    }
}
