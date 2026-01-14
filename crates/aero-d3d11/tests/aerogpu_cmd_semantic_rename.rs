mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::{parse_signatures, DxbcFile, DxbcSignatureParameter, FourCC};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn build_signature_chunk(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name.as_str(),
            semantic_index: p.semantic_index,
            system_value_type: p.system_value_type,
            component_type: p.component_type,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.read_write_mask,
            stream: u32::from(p.stream),
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

fn bytes_of_vertices(verts: &[Vertex]) -> &[u8] {
    // Safety: Vertex is #[repr(C)] and contains only plain f32 arrays with no padding.
    unsafe {
        std::slice::from_raw_parts(verts.as_ptr() as *const u8, core::mem::size_of_val(verts))
    }
}

#[test]
fn aerogpu_cmd_links_vs_ps_by_register_even_when_semantics_rename() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Take the pixel shader DXBC fixture and rename its color input semantic to TEXCOORD0
        // while keeping the same register mapping. This models the Win7 geometry-shader smoke
        // test's pass-through GS, which renames varyings but keeps registers stable.
        let ps_dxbc = {
            let dxbc = DxbcFile::parse(DXBC_PS_PASSTHROUGH).expect("ps fixture should parse");
            let shdr = dxbc
                .get_chunk(FOURCC_SHDR)
                .expect("ps fixture missing SHDR")
                .data
                .to_vec();
            let osgn = dxbc
                .get_chunk(FOURCC_OSGN)
                .expect("ps fixture missing OSGN")
                .data
                .to_vec();
            let sigs = parse_signatures(&dxbc).expect("parse signatures");
            let mut isgn = sigs.isgn.expect("ps fixture missing ISGN").parameters;
            for p in &mut isgn {
                if p.system_value_type == 0
                    && p.semantic_index == 0
                    && p.semantic_name.eq_ignore_ascii_case("COLOR")
                {
                    p.semantic_name = "TEXCOORD".to_owned();
                }
            }
            let isgn_bytes = build_signature_chunk(&isgn);
            build_dxbc(&[
                (FOURCC_SHDR, shdr),
                (FOURCC_ISGN, isgn_bytes),
                (FOURCC_OSGN, osgn),
            ])
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const PS: u32 = 4;
        const IL: u32 = 5;

        // Fullscreen triangle in clip space.
        let verts = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytes_of_vertices(&verts);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, vb_bytes);

        let w = 16u32;
        let h = 16u32;
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::B8G8R8A8Unorm as u32,
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

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_dxbc);

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);

        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: core::mem::size_of::<Vertex>() as u32,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.bind_shaders(VS, PS, 0);

        // Clear to green; the fullscreen triangle should overwrite everything with red.
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 1.0, 0.0, 1.0], 1.0, 0);
        writer.draw(3, 1, 0, 0);
        writer.present(0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("stream should present a render target");
        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}
