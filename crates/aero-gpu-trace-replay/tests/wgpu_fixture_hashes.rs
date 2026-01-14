use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_gpu_trace::{BlobKind, TraceReader, TraceRecord};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn require_webgpu() -> bool {
    let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn frame_hash_bytes(width: u32, height: u32, rgba8: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + rgba8.len());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(rgba8);
    bytes
}

fn extract_cmd_stream(trace_bytes: &[u8]) -> Vec<u8> {
    let mut reader = TraceReader::open(Cursor::new(trace_bytes)).expect("TraceReader::open");
    let entry = reader.frame_entries().first().expect("trace has no frames");
    let records = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .expect("TraceReader::read_records_in_range");

    let mut blobs: HashMap<u64, (BlobKind, Vec<u8>)> = HashMap::new();
    let mut cmd_stream_blob_id = None;

    for record in records {
        match record {
            TraceRecord::Blob {
                blob_id,
                kind,
                bytes,
            } => {
                blobs.insert(blob_id, (kind, bytes));
            }
            TraceRecord::AerogpuSubmission {
                cmd_stream_blob_id: id,
                ..
            } => {
                cmd_stream_blob_id = Some(id);
            }
            _ => {}
        }
    }

    let id = cmd_stream_blob_id.expect("trace missing AerogpuSubmission record");
    let (kind, bytes) = blobs
        .get(&id)
        .unwrap_or_else(|| panic!("trace missing cmd stream blob id {id}"));
    assert_eq!(*kind, BlobKind::AerogpuCmdStream);
    bytes.clone()
}

async fn run_trace_and_hash(trace_bytes: &[u8]) -> Option<(u32, u32, String)> {
    let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
        Ok(exec) => exec,
        Err(e) => {
            skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
            return None;
        }
    };

    let cmd_stream = extract_cmd_stream(trace_bytes);
    let mut guest_mem = VecGuestMemory::new(0x1000);

    let report = exec
        .execute_cmd_stream(&cmd_stream, None, &mut guest_mem)
        .expect("execute_cmd_stream");
    exec.poll_wait();

    let present = report
        .presents
        .last()
        .expect("expected at least one PRESENT in command stream");
    let rt = present
        .presented_render_target
        .expect("PRESENT did not report a presented render target");

    let (width, height) = exec.texture_size(rt).expect("texture_size");
    let rgba8 = exec
        .read_texture_rgba8(rt)
        .await
        .expect("read_texture_rgba8");
    let hash = sha256_hex(&frame_hash_bytes(width, height, &rgba8));
    Some((width, height, hash))
}

#[test]
fn replays_aerogpu_cmd_textured_bc1_triangle_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_textured_bc1_triangle.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "599489cc485b64aa070b1e21e8f3624f6b15c25cb408045db4ef892b3e521c17"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_depth_test_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_depth_test.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "654e833481a9bda84c9a9cccca20a2e1bbe27ae6dbf523c95ee210e85b6916c5"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_constant_buffer_vs_matrix_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_constant_buffer_vs_matrix.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid transparent black (RGBA = [0,0,0,0]).
        assert_eq!(
            hash,
            "644c2a04bcf31fa9c768669a335ee70bd4efba4c445120288f4efeec38d52b5b"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_scissor_test_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_scissor_test.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "7e72d4e6c05012310e548248f838e482b2a741d34d870f92b57431f739d53b5a"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_indexed_triangle_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_indexed_triangle.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "e8f2f09084d6b42df9540e20f141f24810e92482322bbab5c3d17b845a91c572"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_indexed_triangle_u16_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_indexed_triangle_u16.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid blue with alpha=0.
        assert_eq!(
            hash,
            "1dd57f85f575cc48fe0e0e99fef94708b71d5335c9b3ef96cd27991abcccd23d"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_indexed_triangle_base_vertex_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_indexed_triangle_base_vertex.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is a solid green draw with alpha=0.
        assert_eq!(
            hash,
            "cf776eb69882dd61bfb58c0f385347f3d11ac6954b7bbd348cb2c4eb1d33a57d"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_indexed_triangle_first_index_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_indexed_triangle_first_index.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid green with alpha=0.
        assert_eq!(
            hash,
            "cf776eb69882dd61bfb58c0f385347f3d11ac6954b7bbd348cb2c4eb1d33a57d"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_triangle_first_vertex_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_triangle_first_vertex.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid green with alpha=0.
        assert_eq!(
            hash,
            "cf776eb69882dd61bfb58c0f385347f3d11ac6954b7bbd348cb2c4eb1d33a57d"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_triangle_vertex_buffer_offset_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_triangle_vertex_buffer_offset.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid green with alpha=0.
        assert_eq!(
            hash,
            "cf776eb69882dd61bfb58c0f385347f3d11ac6954b7bbd348cb2c4eb1d33a57d"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_blend_add_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_blend_add.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "70b8344e057c7a06b4ce39c0b96f3cc8d4ac92294ed4cf43291272307940d400"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_copy_texture2d_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_copy_texture2d.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "915de25d4d1287576733cf8ea17d77821d20482789218f45ac88dc8fb0231d1f"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_copy_buffer_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_copy_buffer.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "026e53cb3a4319b55b4a231c7ab876ecabf61ac033aae30e920e61feccb3de06"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_textured_rgba8_sampler_triangle_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_textured_rgba8_sampler_triangle.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "f256dd9abeb9918f5169d7250f01ffc7a78c341437b163a7844b2bce99b8062a"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_texture_ld_b5g6r5_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_texture_ld_b5g6r5.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "e271b859f26ca923b6146856a12948a6aadb7923c806309d9ba7e1e6f45cdb57"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_texture_ld_b5g5r5a1_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_texture_ld_b5g5r5a1.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "7476be7554ab0cd0110def15c7b6329b5f25527eb814733788a25a5cec672d64"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_texture_ld_b8g8r8a8_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_texture_ld_b8g8r8a8.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid yellow (BGRA upload interpreted correctly).
        assert_eq!(
            hash,
            "026e53cb3a4319b55b4a231c7ab876ecabf61ac033aae30e920e61feccb3de06"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_clear_b8g8r8x8_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_clear_b8g8r8x8.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid red with opaque alpha.
        assert_eq!(
            hash,
            "1171a4a562614d26797113802f81afae784773e173235286c4f65e4aa1f43816"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_clear_b8g8r8a8_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_clear_b8g8r8a8.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is solid red with *transparent* alpha (A=0).
        assert_eq!(
            hash,
            "cfebc54f35a07ec1e0ca01bf02e8336a69023a3a3dce2dcd34c037281a70a1b9"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_copy_texture2d_subrect_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path(
        "aerogpu_cmd_copy_texture2d_subrect.aerogputrace",
    ))
    .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Expected frame is a red background with a centered green 32x32 square.
        assert_eq!(
            hash,
            "e1fa5ec4ae376ab32cf48b7f06df327e6a50b45cac336c410b39a3685d699a60"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_cull_front_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_cull_front.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        // Cull-front leaves the clear color (opaque black) intact.
        assert_eq!(
            hash,
            "7e78557457b6249af5597fcc8ea8d27f7a4f7f9b4ce58dc9a6b6e6639c7a4207"
        );
    });
}

#[test]
fn replays_aerogpu_cmd_present_ex_fixture_and_matches_hash() {
    let bytes = fs::read(fixture_path("aerogpu_cmd_present_ex.aerogputrace"))
        .expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1 to regenerate");
    pollster::block_on(async {
        let Some((width, height, hash)) = run_trace_and_hash(&bytes).await else {
            return;
        };
        assert_eq!(width, 64);
        assert_eq!(height, 64);
        assert_eq!(
            hash,
            "599489cc485b64aa070b1e21e8f3624f6b15c25cb408045db4ef892b3e521c17"
        );
    });
}
