mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStageEx;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn build_dxbc(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(FourCC, &[u8])> = chunks
        .iter()
        .map(|(fourcc, data)| (FourCC(*fourcc), data.as_slice()))
        .collect();
    dxbc_test_utils::build_container(&refs)
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

        let hs_dxbc = build_dxbc(&[(*b"SHEX", build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(*b"SHEX", build_minimal_sm4_program_chunk(4))]);

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
