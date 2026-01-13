mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundBuffer, BoundConstantBuffer, BoundSampler, ShaderStage};
use aero_d3d11::FourCC;
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuConstantBufferBinding, AerogpuShaderStage, AerogpuShaderStageEx,
    AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
};
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
fn aerogpu_cmd_can_create_and_bind_geometry_shader() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // A minimal DXBC container that parses as a geometry shader (program type 2). WebGPU has
        // no native geometry stage, and the executor only runs a small subset of GS today
        // (point-list prepass). Ensure we accept the shader and allow GS binding/state updates
        // without crashing (unsupported payloads should not trigger stage-mismatch errors).
        let gs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(2))]);

        let mut guest_mem = VecGuestMemory::new(0);
        const BUF: u32 = 10;
        const GS_EX: u32 = 1;
        const GS_LEGACY: u32 = 2;

        // Stream 1: create the shaders + bind GS via the append-only BIND_SHADERS extension.
        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(BUF, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, 16, 0, 0);

        // Create GS via both encodings:
        // - stage_ex extension (stage=Compute, reserved0=DXBC program type tag)
        // - legacy stage enum value (stage=Geometry)
        writer.create_shader_dxbc_ex(GS_EX, AerogpuShaderStageEx::Geometry, &gs_dxbc);
        writer.create_shader_dxbc(GS_LEGACY, AerogpuShaderStage::Geometry, &gs_dxbc);

        writer.bind_shaders_ex(0, 0, 0, GS_EX, 0, 0);
        writer.set_texture_ex(AerogpuShaderStageEx::Geometry, 0, BUF);
        writer.set_samplers_ex(AerogpuShaderStageEx::Geometry, 0, &[123]);
        writer.set_constant_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[AerogpuConstantBufferBinding {
                buffer: BUF,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_shader_constants_f_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[1.0, 2.0, 3.0, 4.0],
        );
        let stream = writer.finish();
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("create shader stream should execute");

        assert_eq!(
            exec.bound_shader_handles().gs,
            Some(GS_EX),
            "BIND_SHADERS extension should bind GS"
        );

        // Stream 2: bind GS via legacy `reserved0` + update GS stage bindings using legacy stage id.
        let mut writer = AerogpuCmdWriter::new();
        writer.bind_shaders_with_gs(0, GS_LEGACY, 0, 0);
        writer.set_texture(AerogpuShaderStage::Geometry, 1, BUF);
        writer.set_samplers(AerogpuShaderStage::Geometry, 1, &[456]);
        writer.set_constant_buffers(
            AerogpuShaderStage::Geometry,
            1,
            &[AerogpuConstantBufferBinding {
                buffer: BUF,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_shader_constants_f(AerogpuShaderStage::Geometry, 4, &[5.0, 6.0, 7.0, 8.0]);
        let stream = writer.finish();
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("update GS stage bindings should succeed");

        assert_eq!(
            exec.shader_stage(GS_EX),
            Some(ShaderStage::Geometry),
            "geometry shader should be accepted and stored"
        );
        assert_eq!(exec.shader_entry_point(GS_EX).unwrap(), "gs_main");
        assert_eq!(exec.shader_stage(GS_LEGACY), Some(ShaderStage::Geometry));
        assert_eq!(exec.shader_entry_point(GS_LEGACY).unwrap(), "gs_main");

        // `BIND_SHADERS.reserved0` should bind GS.
        assert_eq!(exec.bound_shader_handles().gs, Some(GS_LEGACY));

        let gs_bindings = exec.binding_state().stage(ShaderStage::Geometry);
        assert_eq!(
            gs_bindings.srv_buffer(0),
            Some(BoundBuffer {
                buffer: BUF,
                offset: 0,
                size: None
            })
        );
        assert_eq!(
            gs_bindings.srv_buffer(1),
            Some(BoundBuffer {
                buffer: BUF,
                offset: 0,
                size: None
            })
        );
        assert_eq!(gs_bindings.sampler(0), Some(BoundSampler { sampler: 123 }));
        assert_eq!(gs_bindings.sampler(1), Some(BoundSampler { sampler: 456 }));
        assert_eq!(
            gs_bindings.constant_buffer(0),
            Some(BoundConstantBuffer {
                buffer: BUF,
                offset: 0,
                size: None
            })
        );
        assert_eq!(
            gs_bindings.constant_buffer(1),
            Some(BoundConstantBuffer {
                buffer: BUF,
                offset: 0,
                size: None
            })
        );
        assert!(
            exec.binding_state().stage(ShaderStage::Compute).srv_buffer(0).is_none(),
            "GS stage_ex bindings must not clobber compute-stage bindings"
        );

        // Destroying a GS handle should also be accepted (whether or not GS execution is supported).
        let mut writer = AerogpuCmdWriter::new();
        writer.destroy_shader(GS_EX);
        writer.destroy_shader(GS_LEGACY);
        let stream = writer.finish();
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("destroy shader should succeed");
        assert_eq!(exec.shader_stage(GS_EX), None);
        assert_eq!(exec.shader_stage(GS_LEGACY), None);
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
