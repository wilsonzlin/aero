use std::path::PathBuf;

use aero_usb::hid::GamepadReport;
use serde::Deserialize;

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
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/fixtures/hid_gamepad_report_vectors.json");

    let raw = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {fixture_path:?}: {e}"));
    let vectors: Vec<FixtureVector> = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed to parse fixture {fixture_path:?}: {e}"));

    assert!(
        !vectors.is_empty(),
        "fixture {fixture_path:?} should contain at least one vector"
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
                "fixture vector {idx} ({}) has out-of-range axis {axis_name}={} (expected -127..=127)",
                if v.name.is_empty() { "<unnamed>" } else { &v.name },
                axis
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
        let actual = report.to_bytes();
        assert_eq!(
            actual, v.bytes,
            "fixture vector {idx} ({}) mismatch",
            if v.name.is_empty() { "<unnamed>" } else { &v.name }
        );
    }
}
