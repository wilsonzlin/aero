mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuShaderStageEx, AerogpuVertexBufferBinding,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

use aero_d3d11::sm4::opcode::{OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, OPCODE_LEN_SHIFT, OPCODE_RET};

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const DS_TRI_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ds_tri_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

fn build_minimal_hs_dxbc_input_control_points(control_points: u32) -> Vec<u8> {
    // hs_5_0:
    // - dcl_inputcontrolpoints N
    // - ret
    let major = 5u32;
    let minor = 0u32;
    let program_type = 3u32; // HS
    let version = (program_type << 16) | (major << 4) | minor;

    let mut tokens: Vec<u32> = vec![version, 0 /*patched below*/];
    tokens.push(opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2));
    tokens.push(control_points);
    tokens.push(opcode_token(OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    build_dxbc(&[(FOURCC_SHEX, bytes)])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_tessellation_smoke_patchlist3_hs_ds() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_tessellation_smoke_patchlist3_hs_ds"
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
        const VB: u32 = 2;
        const IL: u32 = 3;
        const VS: u32 = 4;
        const PS: u32 = 5;
        const HS: u32 = 6;
        const DS: u32 = 7;

        const W: u32 = 64;
        const H: u32 = 64;

        // Large-ish triangle that does *not* cover the full render target, so we can assert
        // pixels outside it remain at the clear color.
        //
        // Note: order is clockwise to match the executor's default CW front-face state.
        let verts = [
            Vertex {
                pos: [-0.8, -0.8, 0.0],
                // Vertex colors are black so that if tessellation is ignored (VS->PS only),
                // the result stays black and the test fails.
                color: [0.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [0.0, 0.8, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [0.8, -0.8, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];

        let vb_bytes = bytemuck::cast_slice(&verts);
        let vb_size = vb_bytes.len() as u64;
        assert_eq!(vb_size % 4, 0, "vertex buffer must be 4-byte aligned");

        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            W,
            H,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, W as f32, H as f32, 0.0, 1.0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

        writer.create_buffer(VB, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, vb_size, 0, 0);
        writer.upload_resource(VB, 0, vb_bytes);

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

        // Tessellation stages use the `stage_ex` ABI extension.
        let hs_dxbc = build_minimal_hs_dxbc_input_control_points(3);
        writer.create_shader_dxbc_ex(HS, AerogpuShaderStageEx::Hull, &hs_dxbc);
        writer.create_shader_dxbc_ex(DS, AerogpuShaderStageEx::Domain, DS_TRI_PASSTHROUGH);

        // Bind VS+PS and HS/DS (extended append-only BIND_SHADERS payload).
        writer.bind_shaders_ex(VS, PS, 0, 0, HS, DS);

        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: core::mem::size_of::<Vertex>() as u32,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_primitive_topology(AerogpuPrimitiveTopology::PatchList3);

        // One patch, three control points.
        writer.draw(3, 1, 0, 0);

        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                return;
            }
            panic!("execute_cmd_stream should succeed: {err:#}");
        }
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (W * H * 4) as usize);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * W + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // Outside triangle -> clear color (black).
        assert_eq!(px(0, 0), [0, 0, 0, 255]);
        assert_eq!(px(W - 1, 0), [0, 0, 0, 255]);

        // Inside triangle -> barycentric-ish color (DS encodes barycentrics into RGB).
        //
        // At the (approx) center of the triangle we expect one component around 0.5 and the other
        // two around 0.25. We compare sorted RGB channels so the assertion remains correct even if
        // the fixture encodes the barycentric components in a different RGB order.
        let center = px(W / 2, H / 2);
        assert_eq!(center[3], 255, "expected opaque alpha, got {center:?}");

        let mut rgb_sorted = [center[0], center[1], center[2]];
        rgb_sorted.sort();
        let expected_sorted = [64u8, 64u8, 128u8];
        let tol = 40u8;
        let within = rgb_sorted
            .iter()
            .zip(expected_sorted)
            .all(|(&got, exp)| got.abs_diff(exp) <= tol);
        assert!(
            within,
            "expected barycentric-ish center pixel (~[64,64,128] sorted RGB), got {center:?} (sorted {rgb_sorted:?})"
        );
    });
}
