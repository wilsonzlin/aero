mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode as sm4_opcode;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuIndexFormat, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");

const GS_EMIT_TRIANGLE: &[u8] = include_bytes!("fixtures/gs_emit_triangle.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

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
    let mov_token = sm4_opcode::OPCODE_MOV | (8u32 << sm4_opcode::OPCODE_LEN_SHIFT);
    let ret_token = sm4_opcode::OPCODE_RET | (1u32 << sm4_opcode::OPCODE_LEN_SHIFT);

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
    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_linelist_draw_indexed_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_linelist_draw_indexed_translated_prepass"
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

        const VB: u32 = 1;
        const IB: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        // A dummy vertex at index 0 ensures that broken handling of `base_vertex` and/or
        // `first_index` results in a degenerate/cw triangle that does not touch the center pixel.
        let vertices = [
            // v0: dummy (right)
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // v1: right
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // v2: left
            VertexPos3Color4 {
                pos: [-0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        // Indices are arranged so that:
        //   draw_indexed(index_count=2, first_index=2, base_vertex=1) reads [1, 0] -> verts [2, 1]
        // (left->right), producing a CCW triangle that should render.
        //
        // If `first_index` is ignored (reads [0, 0]) or `base_vertex` is ignored (verts [1, 0]),
        // the GS outputs a CW/degenerate triangle that is culled.
        let indices_u16: [u16; 4] = [0, 0, 1, 0];

        let gs_dxbc = common::dxbc_builders::build_gs_linelist_to_triangle_dxbc();
        let ps_dxbc = build_ps_solid_green_dxbc();

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&vertices) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&vertices));

        writer.create_buffer(
            IB,
            AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
            core::mem::size_of_val(&indices_u16) as u64,
            0,
            0,
        );
        writer.upload_resource(IB, 0, bytemuck::cast_slice(&indices_u16));

        // Use an odd-sized render target so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            w,
            h,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);

        // Cull CW triangles (front faces) so placeholder prepass output is culled; the GS emits a
        // CCW triangle using both input line vertices, so only the translated GS prepass should
        // touch the center pixel.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::Front,
            false,
            false,
            0,
            0,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(GS, AerogpuShaderStageEx::Geometry, &gs_dxbc);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_dxbc);

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
        writer.set_index_buffer(IB, AerogpuIndexFormat::Uint16, 0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        // Clear to solid red so we can detect whether the draw actually touched the center pixel.
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw_indexed(2, 1, 2, 1, 0);

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
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let x = w / 2;
        let y = h / 2;
        let idx = ((y * w + x) * 4) as usize;
        let center: [u8; 4] = pixels[idx..idx + 4].try_into().unwrap();

        assert_ne!(
            center,
            [255, 0, 0, 255],
            "center pixel should not match the clear color; translated line-list GS prepass may not have executed"
        );

        // Solid-green PS: center should be green-dominant.
        let [r, g, b, a] = center;
        assert_eq!(a, 255, "expected alpha=255 at center pixel");
        assert!(
            g > r && g > b,
            "expected center pixel to be green-dominant (g > r && g > b), got {center:?}"
        );
    });
}

#[test]
fn aerogpu_cmd_geometry_shader_trianglelist_draw_indexed_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_trianglelist_draw_indexed_translated_prepass"
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

        const VB: u32 = 1;
        const IB: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        // Real triangle vertices are at indices [1..=3]; vertex 0 duplicates vertex 1 so a broken
        // `base_vertex` implementation produces a degenerate triangle that leaves the center pixel
        // untouched.
        let vertices = [
            // v0: dummy = v1
            VertexPos3Color4 {
                pos: [-0.5, -0.5, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            // v1: red
            VertexPos3Color4 {
                pos: [-0.5, -0.5, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            // v2: green
            VertexPos3Color4 {
                pos: [0.0, 0.5, 0.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            // v3: blue
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 1.0, 1.0],
            },
        ];

        // 6 indices so the upload is 4-byte aligned (wgpu write_buffer requirement).
        //
        // draw_indexed(index_count=3, first_index=3, base_vertex=1) reads [0,1,2] -> verts [1,2,3]
        // (the centered triangle).
        let indices_u16: [u16; 6] = [0, 0, 0, 0, 1, 2];

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&vertices) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&vertices));

        writer.create_buffer(
            IB,
            AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
            core::mem::size_of_val(&indices_u16) as u64,
            0,
            0,
        );
        writer.upload_resource(IB, 0, bytemuck::cast_slice(&indices_u16));

        // Use an odd-sized render target so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            w,
            h,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);

        // Disable culling so the emitted triangle is visible regardless of winding.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, GS_EMIT_TRIANGLE);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

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
        writer.set_index_buffer(IB, AerogpuIndexFormat::Uint16, 0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        // Clear to solid red so we can detect whether the draw actually touched the center pixel.
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw_indexed(3, 1, 3, 1, 0);

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
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let x = w / 2;
        let y = h / 2;
        let idx = ((y * w + x) * 4) as usize;
        let center: [u8; 4] = pixels[idx..idx + 4].try_into().unwrap();

        assert_ne!(
            center,
            [255, 0, 0, 255],
            "center pixel should not match the clear color; translated triangle-list GS prepass may not have executed"
        );

        // `gs_emit_triangle` emits a triangle with varying colors; the center should be a
        // green-dominant mix.
        let [r, g, b, a] = center;
        assert_eq!(a, 255, "expected alpha=255 at center pixel");
        assert!(
            g > r && g > b,
            "expected center pixel to be green-dominant (g > r && g > b), got {center:?}"
        );
    });
}
