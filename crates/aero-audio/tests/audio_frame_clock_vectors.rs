use std::path::PathBuf;

use aero_audio::clock::AudioFrameClock;
use serde::Deserialize;

/// Cross-language conformance vectors shared with the browser implementation at
/// `web/src/audio/audio_frame_clock.ts`.
///
/// If `AudioFrameClock` behavior changes intentionally, regenerate
/// `tests/fixtures/audio_frame_clock_vectors.json` via:
///
/// ```bash
/// node tests/fixtures/generate_audio_frame_clock_vectors.mjs > tests/fixtures/audio_frame_clock_vectors.json
/// ```
///
/// and update both the Rust + TypeScript implementations together.
#[test]
fn audio_frame_clock_conformance_vectors() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/audio_frame_clock_vectors.json");
    let json = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read {fixture_path:?}: {e}"));

    let vectors: Vec<Vector> = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("failed to parse {fixture_path:?}: {e}"));
    assert!(
        !vectors.is_empty(),
        "fixture must contain at least one vector"
    );

    for case in vectors {
        assert_eq!(
            case.steps.len(),
            case.expected_frames_per_step.len(),
            "vector {:?} length mismatch: steps has {}, expected_frames_per_step has {}",
            case.name,
            case.steps.len(),
            case.expected_frames_per_step.len()
        );

        let mut clock = AudioFrameClock::new(case.sample_rate_hz, case.start_time_ns);

        for (step_index, (now_ns, expected_frames)) in case
            .steps
            .iter()
            .zip(case.expected_frames_per_step.iter().copied())
            .enumerate()
        {
            let actual = clock.advance_to(*now_ns) as u64;
            assert_eq!(
                actual, expected_frames,
                "vector {:?} step {step_index} (now_ns={now_ns}) frames mismatch",
                case.name
            );
        }

        assert_eq!(
            clock.frac_fp, case.expected_final_frac,
            "vector {:?} end state frac_fp mismatch",
            case.name
        );
    }
}

#[derive(Debug, Deserialize)]
struct Vector {
    name: String,
    sample_rate_hz: u32,
    start_time_ns: u64,
    steps: Vec<u64>,
    expected_frames_per_step: Vec<u64>,
    expected_final_frac: u64,
}
