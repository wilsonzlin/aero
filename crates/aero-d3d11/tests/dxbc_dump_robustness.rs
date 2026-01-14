#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aero_dxbc::test_utils as dxbc_test_utils;
use aero_d3d11::sm4::{FOURCC_SHDR, FOURCC_SHEX};
use aero_d3d11::DxbcFile;

#[test]
fn dxbc_dump_warns_and_truncates_misaligned_shader_chunk() {
    let bin = env!("CARGO_BIN_EXE_dxbc_dump");

    let mut fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fixture.push("tests/fixtures/gs_point_to_triangle.dxbc");
    let fixture_bytes = fs::read(&fixture).expect("read fixture");

    let dxbc = DxbcFile::parse(&fixture_bytes).expect("parse fixture as DXBC");
    let shader_chunk = dxbc
        .get_chunk(FOURCC_SHEX)
        .or_else(|| dxbc.get_chunk(FOURCC_SHDR))
        .expect("fixture is missing SHDR/SHEX shader chunk");

    assert!(
        shader_chunk.data.len().is_multiple_of(4),
        "fixture shader chunk is expected to be 4-byte aligned"
    );

    // Create a minimal DXBC container where the shader chunk payload has an extra padding byte.
    // This is technically malformed, but `dxbc_dump` should warn, truncate back to 4-byte
    // alignment, and continue parsing successfully.
    let mut misaligned_shader = shader_chunk.data.to_vec();
    misaligned_shader.push(0);
    let misaligned = dxbc_test_utils::build_container_unaligned(&[(
        shader_chunk.fourcc,
        misaligned_shader.as_slice(),
    )]);

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let tmp_path = std::env::temp_dir().join(format!("dxbc_dump_misaligned_{unique}.dxbc"));
    fs::write(&tmp_path, &misaligned).expect("write temp dxbc");

    let output = Command::new(bin)
        .arg(&tmp_path)
        .arg("--head")
        .arg("4")
        .output()
        .expect("run dxbc_dump");

    // Best-effort cleanup; ignore errors if the file was already removed.
    let _ = fs::remove_file(&tmp_path);

    assert!(
        output.status.success(),
        "dxbc_dump failed (status={:?})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("warning: shader chunk length"),
        "expected misalignment warning in stdout:\n{stdout}"
    );
    // Sanity checks that truncation still produced a parsable GS token stream.
    assert!(stdout.contains("stage=Geometry"));
    assert!(stdout.contains("(emit)"));
    assert!(stdout.contains("(cut)"));
}
