mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuShaderStageEx,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

use aero_d3d11::sm4::opcode::{OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, OPCODE_LEN_SHIFT, OPCODE_RET};

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
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
    //
    // For HS, include `dcl_inputcontrolpoints` so patchlist validation can proceed far enough for
    // this test to assert the "missing input layout" error (instead of failing earlier on missing
    // HS metadata).
    let mut tokens: Vec<u32> = vec![version, 0 /* patched below */];
    if program_type == 3 {
        tokens.push(opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2));
        tokens.push(3); // PatchList3.
    }
    tokens.push(opcode_token(OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    let mut bytes = Vec::with_capacity(8);
    for t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

#[test]
fn aerogpu_cmd_tessellation_hs_ds_compute_prepass_requires_input_layout() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_tessellation_hs_ds_compute_prepass_requires_input_layout"
        );

        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }

        const RT: u32 = 1;
        const VS: u32 = 2;
        const PS: u32 = 3;
        const HS: u32 = 4;
        const DS: u32 = 5;

        let hs_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(4))]);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            8,
            8,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(HS, AerogpuShaderStageEx::Hull, &hs_dxbc);
        writer.create_shader_dxbc_ex(DS, AerogpuShaderStageEx::Domain, &ds_dxbc);
        writer.bind_shaders_ex(VS, PS, 0, 0, HS, DS);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::PatchList3);
        writer.draw(3, 1, 0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("expected tessellation draw to return an error (not panic)");

        if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
            return;
        }

        let msg = err.to_string();
        assert!(
            msg.contains("tessellation emulation requires an input layout"),
            "unexpected error message: {msg}"
        );
    });
}
