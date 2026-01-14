mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

use aero_d3d11::sm4::opcode::{OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, OPCODE_LEN_SHIFT, OPCODE_RET};

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

fn build_minimal_sm5_program_chunk(program_type: u16, control_points: Option<u32>) -> Vec<u8> {
    // SM5 version token layout:
    // - bits 0..=3: minor version
    // - bits 4..=7: major version
    // - bits 16..=31: program type (0=ps, 1=vs, 2=gs, 3=hs, 4=ds, 5=cs)
    let major = 5u32;
    let minor = 0u32;
    let version = (program_type as u32) << 16 | (major << 4) | minor;

    // Build a minimal token stream, including `dcl_inputcontrolpoints` when requested so patchlist
    // validation can proceed.
    let mut tokens: Vec<u32> = vec![version, 0 /*patched below*/];
    if let Some(control_points) = control_points {
        tokens.push(opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2));
        tokens.push(control_points);
    }
    tokens.push(opcode_token(OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_tessellation_compute_prepass_smoke() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_tessellation_compute_prepass_smoke"
        );
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };
        if !exec.supports_compute() {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }
        if !exec.capabilities().supports_indirect_execution {
            common::skip_or_panic(test_name, "indirect unsupported");
            return;
        }

        // HS/DS emulation relies on compute + indirect execution.
        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }

        const RT: u32 = 1;
        const VB: u32 = 2;
        const VS: u32 = 3;
        const PS: u32 = 4;
        const HS: u32 = 5;
        const DS: u32 = 6;
        const IL: u32 = 7;

        // One triangle patch (3 control points).
        //
        // Keep control point colors black so that if tessellation is ignored (VS->PS only) the
        // output remains black. The current emulation path's placeholder DS encodes barycentric
        // coordinates into COLOR0, so a successful tessellation expansion should produce non-black
        // pixels inside the patch.
        let verts: [VertexPos3Color4; 3] = [
            VertexPos3Color4 {
                pos: [-0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.0, 0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];

        // Minimal DS payload (program type 4 = domain shader). The current emulation path does not
        // execute the translated DS yet, but the executor still validates the stage and stores it.
        let ds_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm5_program_chunk(4, None))]);
        // Minimal HS payload including `dcl_inputcontrolpoints` so patchlist validation succeeds.
        let hs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm5_program_chunk(3, Some(3)))]);

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
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            (core::mem::size_of_val(&verts)) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&verts));

        writer.set_render_targets(&[RT], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(HS, AerogpuShaderStageEx::Hull, &hs_dxbc);
        writer.create_shader_dxbc_ex(DS, AerogpuShaderStageEx::Domain, &ds_dxbc);

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);
        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: core::mem::size_of::<VertexPos3Color4>() as u32,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );

        // Bind HS/DS via the extended shader binding packet. This should route the draw through the
        // tessellation emulation compute-prepass path.
        writer.bind_shaders_ex(VS, PS, 0, 0, HS, DS);

        // Disable face culling so the test does not depend on triangle winding conventions from
        // the tessellator placeholder.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        writer.set_primitive_topology(AerogpuPrimitiveTopology::PatchList3);
        writer.draw(3, 1, 0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                return;
            }
            panic!("execute_cmd_stream failed: {err:#}");
        }
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        let idx = ((8usize / 2) * 8usize + (8usize / 2)) * 4;
        let center = &pixels[idx..idx + 4];
        assert_ne!(center, &[0, 0, 0, 255], "expected non-black center pixel");
    });
}
