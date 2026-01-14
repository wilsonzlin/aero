mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::FourCC;
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
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
fn create_shader_dxbc_accepts_legacy_geometry_stage_value() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Minimal DXBC container that parses as a geometry shader (program type 2).
        let gs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(2))]);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc(1, AerogpuShaderStage::Geometry, &gs_dxbc);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
             .expect("CREATE_SHADER_DXBC (legacy geometry stage=3) should succeed");
    });
}

#[test]
fn create_shader_dxbc_rejects_geometry_stage_mismatch() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // DXBC container that parses as a vertex shader (program type 1), but tagged as geometry.
        // The executor should still validate the DXBC stage.
        let vs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(1))]);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc(1, AerogpuShaderStage::Geometry, &vs_dxbc);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("geometry stage mismatch should be rejected");
        assert!(
            err.to_string().contains("stage mismatch"),
            "unexpected error: {err:#}"
        );
    });
}
