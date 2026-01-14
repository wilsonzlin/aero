mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::sm4::opcode::{OPCODE_DCL_THREAD_GROUP, OPCODE_LEN_SHIFT, OPCODE_RET};
use aero_d3d11::FourCC;
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    let version = ((stage_type as u32) << 16) | (5u32 << 4);
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn build_minimal_compute_dxbc() -> Vec<u8> {
    // Minimal SM5 compute shader:
    //
    // dcl_thread_group 1, 1, 1
    // ret
    //
    // `dcl_thread_group` is required to produce a valid WGSL `@workgroup_size`.
    let tokens = make_sm5_program_tokens(
        5,
        &[
            opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
            1,
            1,
            1,
            opcode_token(OPCODE_RET, 1),
        ],
    );
    build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))])
}

#[test]
fn aerogpu_cmd_accepts_compute_shader_create_and_bind() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let cs_dxbc = build_minimal_compute_dxbc();

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc(1, AerogpuShaderStage::Compute, &cs_dxbc);
        // Bind CS (even if the stream never dispatches).
        writer.bind_shaders(0, 0, 1);
        writer.destroy_shader(1);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("compute shader create/bind should be accepted (even if ignored)");
    });
}

#[test]
fn aerogpu_cmd_draw_with_only_compute_shader_errors_cleanly() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let cs_dxbc = build_minimal_compute_dxbc();

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc(1, AerogpuShaderStage::Compute, &cs_dxbc);
        writer.bind_shaders(0, 0, 1);
        // A draw with only CS bound should return a clear error rather than panicking.
        writer.draw(1, 1, 0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("draw should not succeed without VS/PS");
        let msg = err.to_string();
        assert!(
            msg.contains("use DISPATCH"),
            "unexpected error message: {msg:#}"
        );
    });
}

#[test]
fn aerogpu_cmd_runtime_accepts_compute_shader_dxbc() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        let cs_dxbc = build_minimal_compute_dxbc();
        rt.create_shader_dxbc(1, &cs_dxbc)
            .expect("compute shader DXBC should be accepted/ignored");
    });
}
