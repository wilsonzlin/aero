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

fn assert_rgba_approx(actual: [u8; 4], expected: [u8; 4], tolerance: u8) {
    for (a, e) in actual.into_iter().zip(expected) {
        let diff = a.abs_diff(e);
        assert!(
            diff <= tolerance,
            "component mismatch: actual={actual:?} expected={expected:?} tolerance={tolerance}"
        );
    }
}

#[test]
fn d3d9_runtime_constants_support_ranges_and_vertex_stage() {
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

        const RT: u32 = 1;
        const VB: u32 = 2;

        const TEX_USAGE_RENDER_TARGET: u32 = 1 << 1;
        const BUF_USAGE_VERTEX: u32 = 1 << 0;

        rt.create_texture(
            RT,
            TextureDesc {
                width: 1,
                height: 1,
                mip_level_count: 1,
                format: TextureFormat::Color(ColorFormat::Rgba8Unorm),
                usage: TEX_USAGE_RENDER_TARGET,
            },
        )
        .unwrap();

        // This triangle is entirely off-screen unless the vertex shader applies an offset.
        // The built-in vertex shader uses vertex constants c0.xy + c1.xy as the offset, so we can
        // validate both ranged updates and vertex-stage constant visibility.
        //
        // Use clockwise winding so the default D3D9 cull mode (cull CCW) doesn't discard it.
        let verts: [Vertex; 3] = [
            Vertex {
                pos: [-3.0, -3.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                pos: [-3.0, 1.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                pos: [1.0, -3.0],
                uv: [0.0, 0.0],
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
        rt.set_shader_key(ShaderStage::Fragment, 3).unwrap();

        // Vertex constants: two separate single-register updates.
        rt.set_constants_f32(ShaderStage::Vertex, 0, &[1.0, 1.0, 0.0, 0.0])
            .unwrap();
        rt.set_constants_f32(ShaderStage::Vertex, 1, &[1.0, 1.0, 0.0, 0.0])
            .unwrap();

        // Pixel constants: one ranged update (c0..c1) and one non-contiguous update (c2).
        rt.set_constants_f32(
            ShaderStage::Fragment,
            0,
            &[
                // c0.x supplies red
                1.0, 0.0, 0.0, 0.0, //
                // c1.y supplies green
                0.0, 1.0, 0.0, 0.0, //
            ],
        )
        .unwrap();
        rt.set_constants_f32(
            ShaderStage::Fragment,
            2,
            &[
                // c2.z supplies blue
                0.0, 0.0, 1.0, 0.0, //
            ],
        )
        .unwrap();

        rt.set_render_targets(Some(RenderTarget::Texture(RT)), None)
            .unwrap();
        rt.draw(verts.len() as u32, 0).unwrap();
        rt.present().unwrap();

        let (_, _, got) = rt.readback_texture_rgba8(RT).await.unwrap();
        assert_eq!(got.len(), 4);
        assert_rgba_approx([got[0], got[1], got[2], got[3]], [255, 255, 255, 255], 1);
    });
}

#[test]
fn d3d9_runtime_constants_reject_out_of_range_updates() {
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

        // Last valid register is 255; updating 2 registers starting at 255 should fail.
        let err = rt
            .set_constants_f32(
                ShaderStage::Fragment,
                255,
                &[
                    0.0, 0.0, 0.0, 0.0, //
                    0.0, 0.0, 0.0, 0.0, //
                ],
            )
            .expect_err("expected out-of-range constants update to fail");
        match err {
            RuntimeError::Validation(msg) => assert!(
                msg.contains("out of range") && msg.contains("max_registers=256"),
                "unexpected validation message: {msg}"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
    });
}
