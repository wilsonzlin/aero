mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundConstantBuffer, BoundTexture, ShaderStage};
use aero_d3d11::FourCC;
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuConstantBufferBinding, AerogpuShaderStage, AerogpuShaderStageEx,
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
    // - bits 16..=31: program type (0=ps, 1=vs, 2=gs, 3=hs, 4=ds, 5=cs)
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
fn aerogpu_cmd_create_and_bind_hs_ds_stage_ex() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const HS_SHADER: u32 = 1;
        const DS_SHADER: u32 = 2;
        const CS_SHADER: u32 = 0xDEAD_BEEF;

        let hs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(4))]);

        let mut w = AerogpuCmdWriter::new();
        w.create_shader_dxbc_ex(HS_SHADER, AerogpuShaderStageEx::Hull, &hs_dxbc);
        w.create_shader_dxbc_ex(DS_SHADER, AerogpuShaderStageEx::Domain, &ds_dxbc);

        // Bind HS/DS via the extended BIND_SHADERS payload (`{gs, hs, ds}`).
        w.bind_shaders_ex(
            /*vs=*/ 0, /*ps=*/ 0, /*cs=*/ CS_SHADER, /*gs=*/ 0,
            /*hs=*/ HS_SHADER, /*ds=*/ DS_SHADER,
        );

        let cb = |buffer: u32| AerogpuConstantBufferBinding {
            buffer,
            offset_bytes: 0,
            size_bytes: 16,
            reserved0: 0,
        };

        // Set baseline CS bindings.
        w.set_constant_buffers(AerogpuShaderStage::Compute, 1, &[cb(101)]);
        w.set_texture(AerogpuShaderStage::Compute, 0, 201);

        // HS/DS binding updates must not overwrite CS stage state.
        w.set_constant_buffers_ex(AerogpuShaderStageEx::Hull, 1, &[cb(102)]);
        w.set_texture_ex(AerogpuShaderStageEx::Hull, 0, 202);

        w.set_constant_buffers_ex(AerogpuShaderStageEx::Domain, 1, &[cb(103)]);
        w.set_texture_ex(AerogpuShaderStageEx::Domain, 0, 203);

        // Second CS update ensures CS remains distinct even after HS/DS stage_ex updates.
        w.set_constant_buffers(AerogpuShaderStage::Compute, 1, &[cb(104)]);
        w.set_texture(AerogpuShaderStage::Compute, 0, 204);

        let stream = w.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        // Ensure HS/DS shaders were stored and retain their stage buckets.
        assert_eq!(
            exec.shader_entry_point(HS_SHADER)
                .expect("HS shader should exist after CREATE_SHADER_DXBC"),
            "hs_main"
        );
        assert_eq!(
            exec.shader_entry_point(DS_SHADER)
                .expect("DS shader should exist after CREATE_SHADER_DXBC"),
            "ds_main"
        );
        assert_eq!(
            exec.shader_stage(HS_SHADER)
                .expect("HS shader stage should be stored"),
            ShaderStage::Hull
        );
        assert_eq!(
            exec.shader_stage(DS_SHADER)
                .expect("DS shader stage should be stored"),
            ShaderStage::Domain
        );

        // Ensure BIND_SHADERS set HS/DS and did not clobber CS.
        let bound = exec.bound_shader_handles();
        assert_eq!(bound.cs, Some(CS_SHADER));
        assert_eq!(bound.hs, Some(HS_SHADER));
        assert_eq!(bound.ds, Some(DS_SHADER));

        // Ensure stage_ex binding tables for HS/DS do not clobber CS bindings.
        let bindings = exec.binding_state();
        let expect_cb = |buffer: u32| {
            Some(BoundConstantBuffer {
                buffer,
                offset: 0,
                size: Some(16),
            })
        };

        assert_eq!(
            bindings.stage(ShaderStage::Compute).constant_buffer(1),
            expect_cb(104)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).texture(0),
            Some(BoundTexture { texture: 204 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Hull).constant_buffer(1),
            expect_cb(102)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).texture(0),
            Some(BoundTexture { texture: 202 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Domain).constant_buffer(1),
            expect_cb(103)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).texture(0),
            Some(BoundTexture { texture: 203 })
        );
    });
}
