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

    let vectors: VectorsFile =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("failed to parse {fixture_path:?}: {e}"));

    for case in vectors.cases {
        assert_eq!(
            case.now_ns.len(),
            case.expected_frames.len(),
            "vector case {:?} length mismatch: now_ns has {}, expected_frames has {}",
            case.name,
            case.now_ns.len(),
            case.expected_frames.len()
        );

        let start_time_ns = parse_u64(&case.start_time_ns, "start_time_ns");
        let mut clock = AudioFrameClock::new(case.sample_rate_hz, start_time_ns);

        for (step_index, (now_ns, expected_frames)) in case
            .now_ns
            .iter()
            .zip(case.expected_frames.iter().copied())
            .enumerate()
        {
            let now_ns = parse_u64(now_ns, "now_ns");
            let actual = clock.advance_to(now_ns);
            assert_eq!(
                actual, expected_frames,
                "vector case {:?} step {step_index} (now_ns={now_ns}) frames mismatch",
                case.name
            );
        }

        let expected_last_time_ns = parse_u64(&case.expected_end.last_time_ns, "expected_end.last_time_ns");
        let expected_frac_fp = parse_u64(&case.expected_end.frac_fp, "expected_end.frac_fp");
        assert_eq!(
            clock.last_time_ns, expected_last_time_ns,
            "vector case {:?} end state last_time_ns mismatch",
            case.name
        );
        assert_eq!(
            clock.frac_fp, expected_frac_fp,
            "vector case {:?} end state frac_fp mismatch",
            case.name
        );
    }
}

fn parse_u64(s: &str, field_name: &str) -> u64 {
    s.parse()
        .unwrap_or_else(|e| panic!("invalid u64 for {field_name}: {s:?}: {e}"))
}

#[derive(Debug, Deserialize)]
struct VectorsFile {
    #[allow(dead_code)]
    version: u32,
    cases: Vec<VectorCase>,
}

#[derive(Debug, Deserialize)]
struct VectorCase {
    name: String,
    sample_rate_hz: u32,
    start_time_ns: String,
    now_ns: Vec<String>,
    expected_frames: Vec<usize>,
    expected_end: ExpectedEnd,
}

#[derive(Debug, Deserialize)]
struct ExpectedEnd {
    last_time_ns: String,
    frac_fp: String,
}

