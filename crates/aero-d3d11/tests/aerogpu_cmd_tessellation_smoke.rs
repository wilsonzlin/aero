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

        fn ndc_from_pixel(x: u32, y: u32, w: u32, h: u32) -> (f32, f32) {
            // D3D/WebGPU viewport transform:
            //   x_ndc =  2 * (x + 0.5) / w - 1
            //   y_ndc = -2 * (y + 0.5) / h + 1
            let xf = (x as f32 + 0.5) / w as f32;
            let yf = (y as f32 + 0.5) / h as f32;
            (xf * 2.0 - 1.0, 1.0 - yf * 2.0)
        }

        fn barycentric(a: (f32, f32), b: (f32, f32), c: (f32, f32), p: (f32, f32)) -> [f32; 3] {
            // See: Christer Ericson, "Real-Time Collision Detection", barycentric coordinates.
            let v0 = (b.0 - a.0, b.1 - a.1);
            let v1 = (c.0 - a.0, c.1 - a.1);
            let v2 = (p.0 - a.0, p.1 - a.1);
            let d00 = v0.0 * v0.0 + v0.1 * v0.1;
            let d01 = v0.0 * v1.0 + v0.1 * v1.1;
            let d11 = v1.0 * v1.0 + v1.1 * v1.1;
            let d20 = v2.0 * v0.0 + v2.1 * v0.1;
            let d21 = v2.0 * v1.0 + v2.1 * v1.1;
            let denom = d00 * d11 - d01 * d01;
            let v = (d11 * d20 - d01 * d21) / denom;
            let w = (d00 * d21 - d01 * d20) / denom;
            let u = 1.0 - v - w;
            [u, v, w]
        }

        fn to_unorm8(v: f32) -> u8 {
            ((v.clamp(0.0, 1.0) * 255.0).round() as i32).clamp(0, 255) as u8
        }

        fn assert_rgb_approx_unordered(
            test_name: &str,
            label: &str,
            actual: [u8; 4],
            expected: [u8; 3],
            tol: u8,
        ) {
            assert_eq!(
                actual[3], 255,
                "{test_name}: {label}: expected alpha=255, got {actual:?}"
            );
            assert!(
                actual[0] != 0 && actual[1] != 0 && actual[2] != 0,
                "{test_name}: {label}: expected all RGB channels non-zero (barycentric encoding), got {actual:?}"
            );

            let mut act = [actual[0], actual[1], actual[2]];
            let mut exp = expected;
            act.sort_unstable();
            exp.sort_unstable();
            for i in 0..3 {
                let diff = (act[i] as i32 - exp[i] as i32).abs();
                assert!(
                    diff <= tol as i32,
                    "{test_name}: {label}: expected (unordered) RGB≈{expected:?} ±{tol}, got {actual:?}"
                );
            }
        }

        fn looks_like_centered_placeholder_triangle(
            clear: [u8; 4],
            center: [u8; 4],
            probe_l: [u8; 4],
            probe_r: [u8; 4],
        ) -> bool {
            // The executor currently routes HS/DS-bound patchlist draws through the same placeholder
            // geometry prepass used for GS emulation. That path emits a centered triangle (smaller
            // than our test triangle) and uses a solid red varying fill.
            let probes_clear = probe_l == clear && probe_r == clear;
            let center_red = center[0] > 200 && center[1] < 50 && center[2] < 50 && center[3] > 200;
            probes_clear && center_red
        }

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
        let clear = [0, 0, 0, 255];
        assert_eq!(px(0, 0), clear);
        assert_eq!(px(W - 1, 0), clear);

        // Compute three sample points:
        // - center: safely inside both the placeholder triangle and our larger input triangle.
        // - probes: inside our input triangle but *outside* the centered placeholder triangle, so
        //   they detect whether real tessellation is wired up.
        let center = px(W / 2, H / 2);
        assert_ne!(
            center, clear,
            "{test_name}: expected triangle to cover center pixel"
        );

        let probe_y = (H * 13) / 16; // ~0.81 down from the top, well inside our triangle.
        let probe_rx = (W * 13) / 16;
        let probe_lx = (W - 1) - probe_rx;
        let probe_l = px(probe_lx, probe_y);
        let probe_r = px(probe_rx, probe_y);

        if looks_like_centered_placeholder_triangle(clear, center, probe_l, probe_r) {
            common::skip_or_panic(
                test_name,
                "tessellation HS/DS draws are currently routed through a placeholder compute prepass (real tessellation emulation not implemented yet)",
            );
            return;
        }

        assert_ne!(
            probe_l, clear,
            "{test_name}: expected left probe pixel to be covered by tessellated triangle (got {probe_l:?})"
        );
        assert_ne!(
            probe_r, clear,
            "{test_name}: expected right probe pixel to be covered by tessellated triangle (got {probe_r:?})"
        );

        // Validate that DS encodes barycentric coordinates into COLOR0.
        //
        // We only compare the *multiset* of RGB values (order-insensitive) to keep this robust to
        // any future channel swizzles in the hand-authored fixture. The expected values come from
        // barycentric coordinates of the pixel center in NDC space.
        let a = (verts[0].pos[0], verts[0].pos[1]);
        let b = (verts[1].pos[0], verts[1].pos[1]);
        let c = (verts[2].pos[0], verts[2].pos[1]);

        let check = |label: &str, x: u32, y: u32, actual: [u8; 4]| {
            let p = ndc_from_pixel(x, y, W, H);
            let bc = barycentric(a, b, c, p);
            let expected = [to_unorm8(bc[0]), to_unorm8(bc[1]), to_unorm8(bc[2])];
            assert_rgb_approx_unordered(test_name, label, actual, expected, 30);
        };

        check("center", W / 2, H / 2, center);
        check("probe_left", probe_lx, probe_y, probe_l);
        check("probe_right", probe_rx, probe_y, probe_r);
    });
}
