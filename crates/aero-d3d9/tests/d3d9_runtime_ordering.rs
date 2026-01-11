use aero_d3d9::runtime::{
    ColorFormat, D3D9Runtime, RenderTarget, RuntimeConfig, RuntimeError, ShaderStage, TextureDesc,
    TextureFormat, VertexAttributeDesc, VertexDecl, VertexFormat,
};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

fn require_webgpu() -> bool {
    std::env::var("AERO_REQUIRE_WEBGPU")
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

#[test]
fn d3d9_runtime_preserves_texture_update_draw_ordering() {
    pollster::block_on(async {
        let require_webgpu = require_webgpu();
        let mut rt = match D3D9Runtime::new(RuntimeConfig::default()).await {
            Ok(rt) => rt,
            Err(err @ (RuntimeError::AdapterNotFound | RuntimeError::RequestDevice(_))) => {
                if require_webgpu {
                    panic!("AERO_REQUIRE_WEBGPU is enabled but D3D9Runtime init failed: {err}");
                }
                eprintln!("skipping WebGPU-dependent test: D3D9Runtime init failed: {err}");
                return;
            }
            Err(err) => panic!("D3D9Runtime init failed unexpectedly: {err}"),
        };

        const SRC: u32 = 1;
        const DST1: u32 = 2;
        const DST2: u32 = 3;
        const VB: u32 = 4;

        const TEX_USAGE_SAMPLED: u32 = 1 << 0;
        const TEX_USAGE_RENDER_TARGET: u32 = 1 << 1;
        const BUF_USAGE_VERTEX: u32 = 1 << 0;

        let format = TextureFormat::Color(ColorFormat::Rgba8Unorm);
        rt.create_texture(
            SRC,
            TextureDesc {
                width: 1,
                height: 1,
                mip_level_count: 1,
                format,
                usage: TEX_USAGE_SAMPLED,
            },
        )
        .unwrap();
        for id in [DST1, DST2] {
            rt.create_texture(
                id,
                TextureDesc {
                    width: 1,
                    height: 1,
                    mip_level_count: 1,
                    format,
                    usage: TEX_USAGE_RENDER_TARGET,
                },
            )
            .unwrap();
        }

        let verts: [Vertex; 6] = [
            Vertex {
                pos: [-1.0, -1.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                pos: [1.0, 1.0],
                uv: [1.0, 0.0],
            },
            Vertex {
                pos: [1.0, -1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                pos: [-1.0, -1.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 1.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                pos: [1.0, 1.0],
                uv: [1.0, 0.0],
            },
        ];
        let vb_bytes = bytemuck::cast_slice(&verts);
        rt.create_buffer(VB, vb_bytes.len() as u64, BUF_USAGE_VERTEX)
            .unwrap();
        rt.write_buffer(VB, 0, vb_bytes).unwrap();

        let decl = VertexDecl {
            stride: core::mem::size_of::<Vertex>() as u64,
            attributes: vec![
                VertexAttributeDesc {
                    location: 0,
                    format: VertexFormat::Float32x2,
                    offset: 0,
                },
                VertexAttributeDesc {
                    location: 1,
                    format: VertexFormat::Float32x2,
                    offset: 8,
                },
            ],
        };
        rt.set_vertex_decl(decl.clone()).unwrap();
        rt.set_vertex_stream0(VB, 0, decl.stride).unwrap();

        rt.set_shader_key(ShaderStage::Vertex, 1).unwrap();
        rt.set_shader_key(ShaderStage::Fragment, 2).unwrap();
        rt.set_texture(ShaderStage::Fragment, 0, Some(SRC)).unwrap();

        // Upload A, draw to dst1, upload B, draw to dst2. `write_texture_full_mip` uses
        // `queue.write_texture`, so without an explicit flush it can reorder ahead of the first draw.
        let pattern_a = [255u8, 0u8, 0u8, 255u8];
        let pattern_b = [0u8, 255u8, 0u8, 255u8];

        rt.set_render_targets(Some(RenderTarget::Texture(DST1)), None)
            .unwrap();
        rt.write_texture_full_mip(SRC, 0, 1, 1, &pattern_a).unwrap();
        rt.draw(verts.len() as u32, 0).unwrap();

        rt.write_texture_full_mip(SRC, 0, 1, 1, &pattern_b).unwrap();
        rt.set_render_targets(Some(RenderTarget::Texture(DST2)), None)
            .unwrap();
        rt.draw(verts.len() as u32, 0).unwrap();

        rt.present().unwrap();

        let (_, _, got1) = rt.readback_texture_rgba8(DST1).await.unwrap();
        let (_, _, got2) = rt.readback_texture_rgba8(DST2).await.unwrap();

        assert_eq!(got1.as_slice(), &pattern_a, "dst1 should use pattern A");
        assert_eq!(got2.as_slice(), &pattern_b, "dst2 should use pattern B");
    });
}
