mod common;

use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const DXBC_GS_POINT_TO_TRIANGLE: &[u8] = include_bytes!("fixtures/gs_point_to_triangle.dxbc");

fn build_dxbc(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(FourCC, &[u8])> = chunks
        .iter()
        .map(|(fourcc, data)| (FourCC(*fourcc), data.as_slice()))
        .collect();
    dxbc_test_utils::build_container(&refs)
}

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
    // Mirrors `aero_d3d11::signature::parse_signature_chunk` expectations.
    let mut out = Vec::new();
    out.extend_from_slice(&(params.len() as u32).to_le_bytes()); // param_count
    out.extend_from_slice(&8u32.to_le_bytes()); // param_offset

    let entry_size = 24usize;
    let table_start = out.len();
    out.resize(table_start + params.len() * entry_size, 0);

    let mut strings = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let semantic_name_offset = (8 + params.len() * entry_size + strings.len()) as u32;
        strings.extend_from_slice(p.semantic_name.as_bytes());
        strings.push(0);
        let base = table_start + i * entry_size;
        out[base..base + 4].copy_from_slice(&semantic_name_offset.to_le_bytes());
        out[base + 4..base + 8].copy_from_slice(&p.semantic_index.to_le_bytes());
        out[base + 8..base + 12].copy_from_slice(&0u32.to_le_bytes()); // system_value_type
        out[base + 12..base + 16].copy_from_slice(&0u32.to_le_bytes()); // component_type
        out[base + 16..base + 20].copy_from_slice(&p.register.to_le_bytes());
        out[base + 20] = p.mask;
        out[base + 21] = p.mask; // read_write_mask
        out[base + 22] = 0; // stream
        out[base + 23] = 0; // min_precision
    }
    out.extend_from_slice(&strings);
    out
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn build_vs_pos_only_dxbc() -> Vec<u8> {
    // vs_4_0: mov o0, v0; ret
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x07,
        },
        // Include an unused COLOR0 input so this shader can be paired with the existing
        // POS3+COLOR input-layout fixture.
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Position",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x0001_0040u32; // vs_4_0
    let mov_token = 0x01u32 | (5u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let src_v0 = 0x001e_4016u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0
        src_v0,
        0, // v0
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
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
    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
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
fn gs_test_011_point_to_triangle_emulation_renders_triangle() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::gs_test_011_point_to_triangle_emulation_renders_triangle"
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

         let mut writer = AerogpuCmdWriter::new();

         writer.create_buffer(
             VB,
             AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
             vb_bytes.len() as u64,
             0,
             0,
         );
         writer.upload_resource(VB, 0, vb_bytes);

         let (width, height) = (64u32, 64u32);
         writer.create_texture2d(
             RT,
             AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
             AerogpuFormat::B8G8R8A8Unorm as u32,
             width,
             height,
             1,
             1,
             0,
             0,
             0,
         );

         writer.set_render_targets(&[RT], 0);
         writer.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);

         writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &build_vs_pos_only_dxbc());
         writer.create_shader_dxbc_ex(GS, AerogpuShaderStageEx::Geometry, DXBC_GS_POINT_TO_TRIANGLE);
         writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &build_ps_solid_green_dxbc());

         writer.create_input_layout(IL, ILAY_POS3_COLOR);
         writer.set_input_layout(IL);

         writer.set_primitive_topology(AerogpuPrimitiveTopology::PointList);
         writer.set_vertex_buffers(
             0,
             &[AerogpuVertexBufferBinding {
                 buffer: VB,
                 stride_bytes: 28,
                 offset_bytes: 0,
                 reserved0: 0,
             }],
         );

         writer.bind_shaders_with_gs(VS, GS, PS, 0);
         // Disable face culling so the test does not depend on backend-specific winding conventions.
         writer.set_rasterizer_state_ext(
             AerogpuFillMode::Solid,
             AerogpuCullMode::None,
             false,
             false,
             0,
             false,
         );

         writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
         writer.draw(1, 1, 0, 0);
         writer.present(0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);

          let report = match exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
              Ok(report) => report,
              Err(err) => {
                 if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                      return;
                  }
                  panic!("GS emulation draw failed: {err:#}");
              }
          };

        exec.poll_wait();

        let presented_rt = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("expected a present event");

        let pixels = exec.read_texture_rgba8(presented_rt).await.unwrap();
        assert_eq!(pixels.len(), (width * height * 4) as usize);

         // Center pixel should be shaded green from the emitted triangle.
         let w = width as usize;
         assert_eq!(rgba_at(&pixels, w, (width / 2) as usize, (height / 2) as usize), &[0, 255, 0, 255]);

         // Corner should remain at clear color (red).
         assert_eq!(rgba_at(&pixels, w, 0, 0), &[255, 0, 0, 255]);
     });
 }
