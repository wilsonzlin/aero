use aero_usb::hid::GamepadReport;
use serde::Deserialize;
use std::collections::HashSet;
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
    const MAX_FIXTURE_BYTES: usize = 64 * 1024;

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/fixtures/hid_gamepad_report_vectors.json");

    let raw = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {fixture_path:?}: {e}"));
    assert!(
        raw.len() <= MAX_FIXTURE_BYTES,
        "fixture {fixture_path:?} is too large ({} bytes > {MAX_FIXTURE_BYTES})",
        raw.len()
    );
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

    let mut unique_names = HashSet::<String>::new();

    for (idx, v) in vectors.iter().enumerate() {
        let name = if v.name.is_empty() {
            "<unnamed>"
        } else {
            &v.name
        };
        if !v.name.is_empty() {
            assert!(
                unique_names.insert(v.name.clone()),
                "fixture vector {idx} has duplicate name {:?}",
                v.name
            );
        }

        // Fixture vectors are intended to stay within the canonical report field ranges so they
        // primarily validate the packed layout (endianness + signed byte encoding) rather than
        // clamping behavior.
        assert!(
            v.hat <= 8,
            "fixture vector {idx} ({name}) has out-of-range hat value {} (expected 0..=8)",
            v.hat
        );
        for (axis_name, axis) in [("x", v.x), ("y", v.y), ("rx", v.rx), ("ry", v.ry)] {
            assert!(
                (-127..=127).contains(&axis),
                "fixture vector {idx} ({name}) has out-of-range axis {axis_name}={axis} (expected -127..=127)"
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
        assert_eq!(actual, v.bytes, "fixture vector {idx} ({name}) mismatch");
    }
}
