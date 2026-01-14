mod common;

use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{PrimitiveTopology, RasterizerState, VertexBufferBinding};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_GS_POINT_TO_TRIANGLE: &[u8] = include_bytes!("fixtures/gs_point_to_triangle.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name,
            semantic_index: p.semantic_index,
            system_value_type: 0,
            component_type: 0,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.mask,
            stream: 0,
            min_precision: 0,
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn build_ps_solid_green_dxbc() -> Vec<u8> {
    // ps_4_0: mov o0, l(0,1,0,1); ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_token = 0x01u32 | (8u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;

    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        zero,
        one,
        zero,
        one,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    dxbc_test_utils::build_container_owned(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

fn rgba_at(pixels: &[u8], width: usize, x: usize, y: usize) -> &[u8] {
    let idx = (y * width + x) * 4;
    &pixels[idx..idx + 4]
}

#[test]
fn aerogpu_cmd_runtime_geometry_shader_emulation_renders_triangle() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_runtime_geometry_shader_emulation_renders_triangle"
        );

        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        if !rt.supports_compute() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires compute shaders, but this wgpu backend does not support compute",
            );
            return;
        }
        if !rt.supports_indirect_execution() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires indirect execution, but this wgpu backend does not support indirect draws",
            );
            return;
        }

        // Draw a single point near the top-right. Without GS emulation the point does not cover
        // the center pixel. With GS emulation, the GS emits a centered triangle.
        let vertex = VertexPos3Color4 {
            pos: [0.75, 0.75, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(vb_bytes.len(), 28);

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const GS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(GS, DXBC_GS_POINT_TO_TRIANGLE).unwrap();
        rt.create_shader_dxbc(PS, &build_ps_solid_green_dxbc())
            .unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        rt.create_buffer(
            VB,
            vb_bytes.len() as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, vb_bytes).unwrap();

        let (width, height) = (64u32, 64u32);
        rt.create_texture2d(
            RT,
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RT);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(GS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: 28,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::PointList);
        // Disable face culling so the test does not depend on winding conventions.
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(1, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        // Center pixel should be shaded green from the emitted triangle.
        let w = width as usize;
        assert_eq!(
            rgba_at(&pixels, w, (width / 2) as usize, (height / 2) as usize),
            &[0, 255, 0, 255]
        );

        // Corner should remain at clear color (black).
        assert_eq!(rgba_at(&pixels, w, 0, 0), &[0, 0, 0, 255]);
    });
}
