mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::FourCC;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }
    let total_size = cursor as u32;

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved/unknown
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    for (fourcc, data) in chunks {
        bytes.extend_from_slice(&fourcc.0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }
    assert_eq!(bytes.len(), total_size as usize);
    bytes
}

fn build_minimal_sm4_program_chunk(program_type: u16) -> Vec<u8> {
    // SM4+ version token layout:
    // - bits 0..=3: minor version
    // - bits 4..=7: major version
    // - bits 16..=31: program type (0=ps, 1=vs, 2=gs, ...)
    let major = 4u32;
    let minor = 0u32;
    let version = (program_type as u32) << 16 | (major << 4) | minor;

    // Declared length in DWORDs includes the version + length tokens.
    let declared_len = 2u32;

    let mut bytes = Vec::with_capacity(8);
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&declared_len.to_le_bytes());
    bytes
}

#[test]
fn aerogpu_cmd_ignores_geometry_shader_dxbc_payloads() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // A minimal DXBC container that parses as a geometry shader (program type 2). The command
        // stream only supports VS/PS/CS stages, but we want to accept-and-ignore these payloads
        // rather than failing with a stage-mismatch error.
        let gs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(2))]);

        let mut writer = AerogpuCmdWriter::new();
        // Label it as a vertex shader to simulate a placeholder stage coming from a guest.
        writer.create_shader_dxbc(1, AerogpuShaderStage::Vertex, &gs_dxbc);
        writer.destroy_shader(1);
        let stream = writer.finish();

        let guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &guest_mem)
            .expect("geometry shader DXBC should be ignored, not rejected");
    });
}

#[test]
fn aerogpu_cmd_still_rejects_vertex_pixel_stage_mismatch() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let vs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(1))]);

        let mut writer = AerogpuCmdWriter::new();
        // Submit a vertex shader but label it as pixel stage.
        writer.create_shader_dxbc(2, AerogpuShaderStage::Pixel, &vs_dxbc);
        let stream = writer.finish();

        let guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &guest_mem)
            .expect_err("vertex/pixel stage mismatch should still error");
        assert!(
            err.to_string().contains("stage mismatch"),
            "unexpected error: {err:#}"
        );
    });
}
