use aero_usb::hid::GamepadReport;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct FixtureVector {
    #[serde(default)]
    name: String,
    buttons: i64,
    hat: i64,
    x: i64,
    y: i64,
    rx: i64,
    ry: i64,
    bytes: [u8; 8],
}

fn clamp_buttons_to_u16(buttons: i64) -> u16 {
    // Match TS bitwise semantics (`buttons & 0xffff`) which uses ToInt32.
    let v = buttons as i32;
    (v as u32 & 0xffff) as u16
}

fn clamp_hat_to_u8(hat: i64) -> u8 {
    // Match `packGamepadReport` clamping: valid hat values are 0..=8 inclusive (8 = neutral).
    if (0..=8).contains(&hat) {
        hat as u8
    } else {
        8
    }
}

fn clamp_axis_to_i8(v: i64) -> i8 {
    // Match TS bitwise semantics (`v | 0`) which uses ToInt32, then clamp to the HID logical
    // range [-127, 127].
    let v = (v as i32).clamp(-127, 127);
    v as i8
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
                "fixture vector {idx} ({}) has out-of-range axis {axis_name}={} (expected -127..=127)",
                if v.name.is_empty() { "<unnamed>" } else { &v.name },
                axis
            );
        }

        let report = GamepadReport {
            buttons: clamp_buttons_to_u16(v.buttons),
            hat: clamp_hat_to_u8(v.hat),
            x: clamp_axis_to_i8(v.x),
            y: clamp_axis_to_i8(v.y),
            rx: clamp_axis_to_i8(v.rx),
            ry: clamp_axis_to_i8(v.ry),
        };
        let actual = report.to_bytes();

        let name = if v.name.is_empty() { "<unnamed>" } else { &v.name };
        assert_eq!(actual, v.bytes, "fixture vector {idx} ({name}) mismatch");
    }
}
