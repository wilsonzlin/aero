mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuShaderStageEx, AerogpuVertexBufferBinding,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const HS_TRI_INTEGER: &[u8] = include_bytes!("fixtures/hs_tri_integer.dxbc");
const DS_TRI_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ds_tri_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_tessellation_smoke_patchlist3_hs_ds() {
    pollster::block_on(async {
        let test_name = concat!(module_path!(), "::aerogpu_cmd_tessellation_smoke_patchlist3_hs_ds");
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
        writer.create_shader_dxbc_ex(HS, AerogpuShaderStageEx::Hull, HS_TRI_INTEGER);
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
            let msg = err.to_string();
            if msg.contains("tessellation (HS/DS) compute expansion is not wired up yet") {
                common::skip_or_panic(test_name, "tessellation HS/DS emulation not implemented yet");
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

        // Inside triangle -> non-black (DS encodes barycentrics into color).
        let center = px(W / 2, H / 2);
        assert!(
            center[0] != 0 || center[1] != 0 || center[2] != 0,
            "expected non-black center pixel, got {center:?}"
        );
    });
}
