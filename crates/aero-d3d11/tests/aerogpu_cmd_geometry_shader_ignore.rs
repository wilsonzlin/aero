mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::ShaderStage;
use aero_d3d11::FourCC;
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{AerogpuShaderStage, AerogpuShaderStageEx};
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
fn aerogpu_cmd_accepts_geometry_shader_stage_ex_plumbing() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // A minimal DXBC container that parses as a geometry shader (program type 2).
        let gs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(2))]);

        let mut writer = AerogpuCmdWriter::new();
        // Encode GS creation and binding updates using the "stage_ex" ABI extension so we exercise
        // the extended-stage plumbing even on older hosts.
        writer.create_shader_dxbc_ex(1, AerogpuShaderStageEx::Geometry, &gs_dxbc);
        // Ensure geometry-stage bindings are accepted (even though WebGPU has no GS stage).
        writer.bind_shaders_with_gs(0, 1, 0, 0);
        writer.set_texture_ex(AerogpuShaderStageEx::Geometry, 0, 0);
        writer.set_shader_constants_f_ex(AerogpuShaderStageEx::Geometry, 0, &[1.0, 2.0, 3.0, 4.0]);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("create shader stream should execute");
        assert_eq!(
            exec.shader_stage(1),
            Some(ShaderStage::Geometry),
            "geometry shader should be accepted and stored"
        );
        assert_eq!(
            exec.shader_entry_point(1).expect("shader should exist"),
            "gs_main"
        );

        assert_eq!(exec.bound_shader_handles().gs, Some(1));

        // Destroying a GS handle should also be accepted (whether or not GS execution is supported).
        let mut writer = AerogpuCmdWriter::new();
        writer.destroy_shader(1);
        let stream = writer.finish();
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("destroy shader should succeed");
        assert_eq!(exec.shader_stage(1), None);
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

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("vertex/pixel stage mismatch should still error");
        assert!(
            err.to_string().contains("stage mismatch"),
            "unexpected error: {err:#}"
        );
    });
}
