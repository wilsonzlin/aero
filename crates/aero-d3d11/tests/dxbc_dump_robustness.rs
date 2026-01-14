#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aero_d3d11::sm4::{FOURCC_SHDR, FOURCC_SHEX};
use aero_d3d11::{DxbcFile, FourCC};

fn push_u32_le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn make_single_chunk_dxbc(fourcc: FourCC, data: &[u8]) -> Vec<u8> {
    // Minimal DXBC container:
    // - header (32 bytes)
    // - 1 chunk offset (4 bytes)
    // - chunk header (8 bytes)
    // - chunk payload (variable)
    let header_len = 4 + 16 + 4 + 4 + 4;
    let chunk_count = 1u32;
    let offset_table_len = (chunk_count as usize) * 4;
    let chunk_offset = (header_len + offset_table_len) as u32;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (unused by our parser)
    push_u32_le(&mut bytes, 0); // reserved
    let total_size_pos = bytes.len();
    push_u32_le(&mut bytes, 0); // placeholder total_size
    push_u32_le(&mut bytes, chunk_count);
    push_u32_le(&mut bytes, chunk_offset);

    assert_eq!(bytes.len(), chunk_offset as usize);

    bytes.extend_from_slice(&fourcc.0);
    push_u32_le(&mut bytes, data.len() as u32);
    bytes.extend_from_slice(data);

    let total_size = bytes.len() as u32;
    bytes[total_size_pos..total_size_pos + 4].copy_from_slice(&total_size.to_le_bytes());
    bytes
}

#[test]
fn dxbc_dump_warns_and_truncates_misaligned_shader_chunk() {
    let bin = env!("CARGO_BIN_EXE_dxbc_dump");

    let mut fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fixture.push("tests/fixtures/gs_emit_cut.dxbc");
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
    let misaligned = make_single_chunk_dxbc(shader_chunk.fourcc, &misaligned_shader);

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

