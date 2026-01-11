use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/aerogpu_cmd_triangle.aerogputrace")
}

#[test]
fn replays_aerogpu_cmd_triangle_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path())
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    let frames = aero_gpu_trace_replay::replay_trace(Cursor::new(bytes)).expect("replay trace");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].width, 64);
    assert_eq!(frames[0].height, 64);

    // This hash is intentionally stable: it hashes (width,height,rgba8) using SHA-256.
    assert_eq!(
        frames[0].sha256(),
        "1171a4a562614d26797113802f81afae784773e173235286c4f65e4aa1f43816"
    );
}
