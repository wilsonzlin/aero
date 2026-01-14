mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::shader_lib::vertex_pulling::{
    IaMeta, IA_BINDING_META, IA_BINDING_VERTEX_BUFFER_BASE, IA_BINDING_VERTEX_BUFFER_END,
    IA_MAX_VERTEX_BUFFERS, WGSL as VERTEX_PULLING_WGSL,
};
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, PipelineKind, ShaderStageFlags,
};

fn push_f32(dst: &mut Vec<u8>, v: f32) {
    dst.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(dst: &mut Vec<u8>, v: u32) {
    dst.extend_from_slice(&v.to_le_bytes());
}

fn assert_f32_near(actual: f32, expected: f32, label: &str) {
    let diff = (actual - expected).abs();
    assert!(
        diff <= 1e-5,
        "{label}: expected {expected}, got {actual} (diff={diff})"
    );
}

#[test]
fn compute_can_vertex_pull_common_formats_from_multiple_vertex_buffers() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_can_vertex_pull_common_formats_from_multiple_vertex_buffers"
        );
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        if !rt.supports_compute() {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }

        const VB0: u32 = 1;
        const VB1: u32 = 2;
        const META: u32 = 3;
        const OUT: u32 = 4;
        const SHADER: u32 = 5;
        const PIPELINE: u32 = 6;

        // Two vertices:
        // - vb0 (slot 0): POSITION float3
        // - vb1 (slot 1): TEXCOORD float2 + COLOR R8G8B8A8_UNORM (with a 16-byte prefix to test base offsets)
        let mut vb0 = Vec::<u8>::new();
        // v0 position
        push_f32(&mut vb0, 1.0);
        push_f32(&mut vb0, 2.0);
        push_f32(&mut vb0, 3.0);
        // v1 position
        push_f32(&mut vb0, 4.0);
        push_f32(&mut vb0, 5.0);
        push_f32(&mut vb0, 6.0);
        assert_eq!(vb0.len(), 24);

        let mut vb1 = vec![0u8; 16]; // prefix padding
                                     // v0 uv
        push_f32(&mut vb1, 0.25);
        push_f32(&mut vb1, 0.5);
        // v0 color (R=128, G=64, B=255, A=0)
        push_u32(&mut vb1, 0x00FF_4080);
        // v1 uv
        push_f32(&mut vb1, 0.75);
        push_f32(&mut vb1, 1.0);
        // v1 color (R=0, G=255, B=0, A=255)
        push_u32(&mut vb1, 0xFF00_FF00);
        assert_eq!(vb1.len(), 16 + 12 * 2);

        let mut meta = IaMeta::default();
        meta.vb[0].base_offset_bytes = 0;
        meta.vb[0].stride_bytes = 12;
        meta.vb[1].base_offset_bytes = 16;
        meta.vb[1].stride_bytes = 12;

        let output_vec4s = 2u32 * 3u32; // (pos, uv, color) per vertex
        let output_size = output_vec4s as u64 * 16;

        let wgsl = format!(
            r#"
{VERTEX_PULLING_WGSL}

struct OutBuf {{
  data: array<vec4<f32>>,
}};

@group(2) @binding({out_binding}) var<storage, read_write> out_buf: OutBuf;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  let idx = gid.x;
  let base = idx * 3u;

  let pos = ia_load_r32g32b32_float(0u, idx, 0u);
  let uv = ia_load_r32g32_float(1u, idx, 0u);
  let col = ia_load_r8g8b8a8_unorm(1u, idx, 8u);

  out_buf.data[base + 0u] = vec4<f32>(pos, 1.0);
  out_buf.data[base + 1u] = vec4<f32>(uv, 0.0, 1.0);
  out_buf.data[base + 2u] = col;
}}
"#,
            out_binding = IA_BINDING_VERTEX_BUFFER_END
        );

        let mut bindings: Vec<BindingDesc> = Vec::new();
        bindings.push(BindingDesc {
            binding: IA_BINDING_META,
            ty: BindingType::UniformBuffer,
            visibility: ShaderStageFlags::COMPUTE,
            storage_texture_format: None,
        });
        for i in 0..IA_MAX_VERTEX_BUFFERS as u32 {
            bindings.push(BindingDesc {
                binding: IA_BINDING_VERTEX_BUFFER_BASE + i,
                ty: BindingType::StorageBufferReadOnly,
                visibility: ShaderStageFlags::COMPUTE,
                storage_texture_format: None,
            });
        }
        bindings.push(BindingDesc {
            binding: IA_BINDING_VERTEX_BUFFER_END,
            ty: BindingType::StorageBufferReadWrite,
            visibility: ShaderStageFlags::COMPUTE,
            storage_texture_format: None,
        });

        let mut writer = CmdWriter::new();
        // Deliberately omit BufferUsage::STORAGE for vertex buffers; the runtime should still add
        // `wgpu::BufferUsages::STORAGE` so IA buffers are vertex-pullable in internal compute
        // prepasses.
        writer.create_buffer(VB0, vb0.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(VB1, vb1.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(META, meta.as_bytes().len() as u64, BufferUsage::UNIFORM);
        writer.create_buffer(
            OUT,
            output_size,
            BufferUsage::STORAGE | BufferUsage::MAP_READ,
        );

        writer.update_buffer(VB0, 0, &vb0);
        writer.update_buffer(VB1, 0, &vb1);
        writer.update_buffer(META, 0, meta.as_bytes());

        writer.create_shader_module_wgsl(SHADER, &wgsl);
        writer.create_compute_pipeline(PIPELINE, SHADER, &bindings);

        writer.begin_compute_pass();
        writer.set_pipeline(PipelineKind::Compute, PIPELINE);

        writer.set_bind_buffer(IA_BINDING_META, META, 0, 0);
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + 0, VB0, 0, 0);
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + 1, VB1, 0, 0);
        // Bind unused slots to a valid buffer; the shader doesn't reference them for this test.
        for i in 2..IA_MAX_VERTEX_BUFFERS as u32 {
            writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + i, VB0, 0, 0);
        }
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_END, OUT, 0, 0);

        writer.dispatch(2, 1, 1);
        writer.end_compute_pass();

        rt.execute(&writer.finish())
            .expect("compute dispatch with vertex pulling should succeed");

        let bytes = rt
            .read_buffer(OUT, 0, output_size)
            .await
            .expect("read output buffer");

        assert_eq!(
            bytes.len(),
            output_size as usize,
            "unexpected output buffer size"
        );

        let mut floats = Vec::<f32>::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            floats.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        assert_eq!(floats.len(), (output_vec4s * 4) as usize);

        let vec4 = |idx: usize| -> [f32; 4] {
            let base = idx * 4;
            [
                floats[base],
                floats[base + 1],
                floats[base + 2],
                floats[base + 3],
            ]
        };

        let expected = [
            // v0 pos
            [1.0, 2.0, 3.0, 1.0],
            // v0 uv
            [0.25, 0.5, 0.0, 1.0],
            // v0 color (128,64,255,0) / 255
            [128.0 / 255.0, 64.0 / 255.0, 1.0, 0.0],
            // v1 pos
            [4.0, 5.0, 6.0, 1.0],
            // v1 uv
            [0.75, 1.0, 0.0, 1.0],
            // v1 color (0,255,0,255) / 255
            [0.0, 1.0, 0.0, 1.0],
        ];

        for (i, exp) in expected.iter().enumerate() {
            let got = vec4(i);
            for lane in 0..4 {
                assert_f32_near(got[lane], exp[lane], &format!("vec4[{i}].{lane}"));
            }
        }
    });
}
