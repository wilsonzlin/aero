mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStageEx;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const HS_RET: &[u8] = include_bytes!("fixtures/hs_ret.dxbc");
const DS_RET: &[u8] = include_bytes!("fixtures/ds_ret.dxbc");

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
fn aerogpu_cmd_create_shader_dxbc_stage_ex_stores_hs_ds() {
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

        let hs_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(4))]);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc_ex(HS_SHADER, AerogpuShaderStageEx::Hull, &hs_dxbc);
        writer.create_shader_dxbc_ex(DS_SHADER, AerogpuShaderStageEx::Domain, &ds_dxbc);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        assert_eq!(exec.shader_entry_point(HS_SHADER).unwrap(), "hs_main");
        assert_eq!(exec.shader_entry_point(DS_SHADER).unwrap(), "ds_main");
    });
}

#[test]
fn aerogpu_cmd_create_shader_dxbc_stage_ex_rejects_zero_handle() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Minimal HS DXBC container (program type 3).
        let hs_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(3))]);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc_ex(0, AerogpuShaderStageEx::Hull, &hs_dxbc);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("shader_handle==0 should be rejected");
        assert!(
            err.to_string().contains("invalid shader_handle 0"),
            "unexpected error: {err:#}"
        );
    });
}

#[test]
fn aerogpu_cmd_create_shader_dxbc_stage_ex_rejects_empty_payload() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc_ex(1, AerogpuShaderStageEx::Hull, &[]);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("empty DXBC payload should be rejected");
        assert!(
            err.to_string().contains("empty DXBC payload"),
            "unexpected error: {err:#}"
        );
    });
}

#[test]
fn aerogpu_cmd_create_shader_dxbc_stage_ex_stage_mismatch_includes_stage_ex() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // HS DXBC but encoded as a DS stage-ex packet.
        let hs_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(3))]);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc_ex(1, AerogpuShaderStageEx::Domain, &hs_dxbc);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("stage mismatch should error");
        let msg = err.to_string();
        assert!(
            msg.contains("stage mismatch"),
            "unexpected error (missing stage mismatch): {err:#}"
        );
        assert!(
            msg.contains("stage_ex=4"),
            "expected stage_ex value in error message, got: {msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_create_shader_dxbc_stage_ex_translates_hs_ds_when_signatures_present() {
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

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc_ex(HS_SHADER, AerogpuShaderStageEx::Hull, HS_RET);
        writer.create_shader_dxbc_ex(DS_SHADER, AerogpuShaderStageEx::Domain, DS_RET);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        let hs_wgsl = exec
            .shader_wgsl_source(HS_SHADER)
            .expect("HS shader should exist after CREATE_SHADER_DXBC");
        assert!(
            hs_wgsl.contains("fn hs_patch_constants"),
            "expected translated HS WGSL to include patch-constant entry point:\n{hs_wgsl}"
        );

        let ds_wgsl = exec
            .shader_wgsl_source(DS_SHADER)
            .expect("DS shader should exist after CREATE_SHADER_DXBC");
        assert!(
            ds_wgsl.contains("ds_in_cp"),
            "expected translated DS WGSL to include control-point input buffer plumbing:\n{ds_wgsl}"
        );
    });
}
