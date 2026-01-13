use aero_usb::hid::GamepadReport;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct FixtureVector {
    #[serde(default)]
    name: String,
    buttons: u16,
    hat: u8,
    x: i8,
    y: i8,
    rx: i8,
    ry: i8,
    bytes: [u8; 8],
}

#[test]
fn hid_gamepad_report_vectors_match_fixture() {
    const MAX_FIXTURE_VECTORS: usize = 64;

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/fixtures/hid_gamepad_report_vectors.json");

    let raw = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {fixture_path:?}: {e}"));
    let vectors: Vec<FixtureVector> = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed to parse fixture {fixture_path:?}: {e}"));

    assert!(
        !vectors.is_empty(),
        "fixture {fixture_path:?} should contain at least one vector"
    );
    assert!(
        vectors.len() <= MAX_FIXTURE_VECTORS,
        "fixture {fixture_path:?} is too large ({} > {MAX_FIXTURE_VECTORS})",
        vectors.len()
    );

    for (idx, v) in vectors.iter().enumerate() {
        assert!(
            v.hat <= 8,
            "fixture vector {idx} ({}) has out-of-range hat value {} (expected 0..=8)",
            if v.name.is_empty() { "<unnamed>" } else { &v.name },
            v.hat
        );
        for (axis_name, axis) in [("x", v.x), ("y", v.y), ("rx", v.rx), ("ry", v.ry)] {
            assert!(
                (-127..=127).contains(&axis),
                "fixture vector {idx} ({}) has out-of-range axis {axis_name}={axis} (expected -127..=127)",
                if v.name.is_empty() { "<unnamed>" } else { &v.name },
            );
        }

        let report = GamepadReport {
            buttons: v.buttons,
            hat: v.hat,
            x: v.x,
            y: v.y,
            rx: v.rx,
            ry: v.ry,
        };
        // The fixture intentionally includes out-of-range hat/axis values to validate our clamping
        // behaviour, so we only assert on the clamped values.
        assert!(
            report.hat <= 8,
            "fixture vector {idx} ({}) clamped to out-of-range hat value {} (expected 0..=8)",
            if v.name.is_empty() { "<unnamed>" } else { &v.name },
            report.hat
        );
        for (axis_name, axis) in [("x", report.x), ("y", report.y), ("rx", report.rx), ("ry", report.ry)] {
            assert!(
                (-127..=127).contains(&(axis as i16)),
                "fixture vector {idx} ({}) clamped to out-of-range axis {axis_name}={} (expected -127..=127)",
                if v.name.is_empty() { "<unnamed>" } else { &v.name },
                axis
            );
        }
        let actual = report.to_bytes();

        let name = if v.name.is_empty() { "<unnamed>" } else { &v.name };
        assert_eq!(actual, v.bytes, "fixture vector {idx} ({name}) mismatch");
    }
}
