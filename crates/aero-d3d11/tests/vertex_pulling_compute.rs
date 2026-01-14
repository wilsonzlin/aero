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

fn push_u16(dst: &mut Vec<u8>, v: u16) {
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
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE, VB0, 0, 0);
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

#[test]
fn compute_vertex_pulling_supports_unaligned_byte_addresses() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_vertex_pulling_supports_unaligned_byte_addresses"
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

        const VB: u32 = 1;
        const META: u32 = 2;
        const OUT: u32 = 3;
        const SHADER: u32 = 4;
        const PIPELINE: u32 = 5;

        // Vertex buffer bytes (12 bytes, padded to u32 alignment):
        //
        // We intentionally choose an unaligned base offset (1 byte) so the shader must stitch two u32
        // reads to reconstruct the requested dword.
        //
        // At byte offset 1: [0x11, 0x22, 0x33, 0x44] => 0x44332211
        // At byte offset 5: [0x55, 0x66, 0x77, 0x88] => 0x88776655
        let vb = vec![
            0x00u8, // padding byte (unaddressed by the shader)
            0x11u8, 0x22u8, 0x33u8, 0x44u8, 0x55u8, 0x66u8, 0x77u8, 0x88u8, 0x00u8, 0x00u8, 0x00u8,
        ];

        let mut meta = IaMeta::default();
        meta.vb[0].base_offset_bytes = 1;
        meta.vb[0].stride_bytes = 4;

        // Output buffer: 2 dwords.
        let output_size = 8u64;

        let wgsl = format!(
            r#"
{VERTEX_PULLING_WGSL}

struct OutBuf {{
  data: array<u32>,
}};

@group(2) @binding({out_binding}) var<storage, read_write> out_buf: OutBuf;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  if (gid.x != 0u) {{ return; }}
  let a0 = ia_load_u32(0u, ia_vertex_byte_addr(0u, 0u, 0u));
  let a1 = ia_load_u32(0u, ia_vertex_byte_addr(0u, 1u, 0u));
  out_buf.data[0u] = a0;
  out_buf.data[1u] = a1;
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
        writer.create_buffer(VB, vb.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(META, meta.as_bytes().len() as u64, BufferUsage::UNIFORM);
        writer.create_buffer(
            OUT,
            output_size,
            BufferUsage::STORAGE | BufferUsage::MAP_READ,
        );

        writer.update_buffer(VB, 0, &vb);
        writer.update_buffer(META, 0, meta.as_bytes());

        writer.create_shader_module_wgsl(SHADER, &wgsl);
        writer.create_compute_pipeline(PIPELINE, SHADER, &bindings);

        writer.begin_compute_pass();
        writer.set_pipeline(PipelineKind::Compute, PIPELINE);

        writer.set_bind_buffer(IA_BINDING_META, META, 0, 0);
        for i in 0..IA_MAX_VERTEX_BUFFERS as u32 {
            writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + i, VB, 0, 0);
        }
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_END, OUT, 0, 0);

        writer.dispatch(1, 1, 1);
        writer.end_compute_pass();

        rt.execute(&writer.finish())
            .expect("compute dispatch with vertex pulling should succeed");

        let bytes = rt
            .read_buffer(OUT, 0, output_size)
            .await
            .expect("read output buffer");
        assert_eq!(bytes.len(), output_size as usize);

        let got0 = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let got1 = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(got0, 0x4433_2211);
        assert_eq!(got1, 0x8877_6655);
    });
}

#[test]
fn compute_can_vertex_pull_f16_u16_u32_formats() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_can_vertex_pull_f16_u16_u32_formats"
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

        const VB_F16: u32 = 1;
        const VB_U16: u32 = 2;
        const VB_U32: u32 = 3;
        const META: u32 = 4;
        const OUT: u32 = 5;
        const SHADER: u32 = 6;
        const PIPELINE: u32 = 7;

        // Two vertices:
        // - vb0: DXGI_FORMAT_R16G16_FLOAT (half2) + DXGI_FORMAT_R16_FLOAT (scalar, tested via offsets)
        // - vb1: DXGI_FORMAT_R16_UINT (u16, padded to 4 bytes per vertex)
        // - vb2: DXGI_FORMAT_R32_UINT (u32) + vectors (tested via ia_load_r32g32/… helpers)
        //
        // Half float bit patterns (IEEE-754 binary16):
        // - 1.0  = 0x3C00
        // - 0.5  = 0x3800
        // - 2.0  = 0x4000
        // - -1.0 = 0xBC00
        let mut vb_f16 = Vec::<u8>::new();
        // v0 half2 = (1.0, 0.5)
        push_u16(&mut vb_f16, 0x3C00);
        push_u16(&mut vb_f16, 0x3800);
        // v1 half2 = (2.0, -1.0)
        push_u16(&mut vb_f16, 0x4000);
        push_u16(&mut vb_f16, 0xBC00);
        assert_eq!(vb_f16.len(), 8);

        let mut vb_u16 = Vec::<u8>::new();
        // v0 u16 = 1234, plus 2 bytes padding
        push_u16(&mut vb_u16, 1234);
        push_u16(&mut vb_u16, 0);
        // v1 u16 = 65535, plus 2 bytes padding
        push_u16(&mut vb_u16, 65535);
        push_u16(&mut vb_u16, 0);
        assert_eq!(vb_u16.len(), 8);

        let mut vb_u32 = Vec::<u8>::new();
        // v0 u32x4 = (42, 43, 44, 45)
        push_u32(&mut vb_u32, 42);
        push_u32(&mut vb_u32, 43);
        push_u32(&mut vb_u32, 44);
        push_u32(&mut vb_u32, 45);
        // v1 u32x4 = (1000, 1001, 1002, 1003)
        push_u32(&mut vb_u32, 1000);
        push_u32(&mut vb_u32, 1001);
        push_u32(&mut vb_u32, 1002);
        push_u32(&mut vb_u32, 1003);
        assert_eq!(vb_u32.len(), 32);

        let mut meta = IaMeta::default();
        meta.vb[0].base_offset_bytes = 0;
        meta.vb[0].stride_bytes = 4;
        meta.vb[1].base_offset_bytes = 0;
        meta.vb[1].stride_bytes = 4;
        meta.vb[2].base_offset_bytes = 0;
        meta.vb[2].stride_bytes = 16;

        let output_vec4s = 2u32 * 8u32; // 8 vec4s per vertex
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
  let base = idx * 8u;

  let h = ia_load_r16g16_float(0u, idx, 0u);
  let h0 = ia_load_r16_float(0u, idx, 0u);
  let h1 = ia_load_r16_float(0u, idx, 2u);
  let u16v = ia_load_r16_uint(1u, idx, 0u);
  let u32v = ia_load_r32_uint(2u, idx, 0u);
  let u32v2 = ia_load_r32g32_uint(2u, idx, 0u);
  let u32v3 = ia_load_r32g32b32_uint(2u, idx, 0u);
  let u32v4 = ia_load_r32g32b32a32_uint(2u, idx, 0u);

  out_buf.data[base + 0u] = vec4<f32>(h, 0.0, 1.0);
  out_buf.data[base + 1u] = vec4<f32>(h0, 0.0, 0.0, 1.0);
  out_buf.data[base + 2u] = vec4<f32>(h1, 0.0, 0.0, 1.0);
  out_buf.data[base + 3u] = vec4<f32>(f32(u16v), 0.0, 0.0, 1.0);
  out_buf.data[base + 4u] = vec4<f32>(f32(u32v), 0.0, 0.0, 1.0);
  out_buf.data[base + 5u] = vec4<f32>(f32(u32v2.x), f32(u32v2.y), 0.0, 1.0);
  out_buf.data[base + 6u] = vec4<f32>(f32(u32v3.x), f32(u32v3.y), f32(u32v3.z), 1.0);
  out_buf.data[base + 7u] = vec4<f32>(f32(u32v4.x), f32(u32v4.y), f32(u32v4.z), f32(u32v4.w));
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
        writer.create_buffer(VB_F16, vb_f16.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(VB_U16, vb_u16.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(VB_U32, vb_u32.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(META, meta.as_bytes().len() as u64, BufferUsage::UNIFORM);
        writer.create_buffer(
            OUT,
            output_size,
            BufferUsage::STORAGE | BufferUsage::MAP_READ,
        );

        writer.update_buffer(VB_F16, 0, &vb_f16);
        writer.update_buffer(VB_U16, 0, &vb_u16);
        writer.update_buffer(VB_U32, 0, &vb_u32);
        writer.update_buffer(META, 0, meta.as_bytes());

        writer.create_shader_module_wgsl(SHADER, &wgsl);
        writer.create_compute_pipeline(PIPELINE, SHADER, &bindings);

        writer.begin_compute_pass();
        writer.set_pipeline(PipelineKind::Compute, PIPELINE);

        writer.set_bind_buffer(IA_BINDING_META, META, 0, 0);
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE, VB_F16, 0, 0);
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + 1, VB_U16, 0, 0);
        writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + 2, VB_U32, 0, 0);
        for i in 3..IA_MAX_VERTEX_BUFFERS as u32 {
            writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + i, VB_F16, 0, 0);
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
            // v0 half2
            [1.0, 0.5, 0.0, 1.0],
            // v0 r16_float @ offset 0
            [1.0, 0.0, 0.0, 1.0],
            // v0 r16_float @ offset 2 (unaligned)
            [0.5, 0.0, 0.0, 1.0],
            // v0 u16
            [1234.0, 0.0, 0.0, 1.0],
            // v0 u32
            [42.0, 0.0, 0.0, 1.0],
            // v0 u32x2
            [42.0, 43.0, 0.0, 1.0],
            // v0 u32x3
            [42.0, 43.0, 44.0, 1.0],
            // v0 u32x4
            [42.0, 43.0, 44.0, 45.0],
            // v1 half2
            [2.0, -1.0, 0.0, 1.0],
            // v1 r16_float @ offset 0
            [2.0, 0.0, 0.0, 1.0],
            // v1 r16_float @ offset 2 (unaligned)
            [-1.0, 0.0, 0.0, 1.0],
            // v1 u16
            [65535.0, 0.0, 0.0, 1.0],
            // v1 u32
            [1000.0, 0.0, 0.0, 1.0],
            // v1 u32x2
            [1000.0, 1001.0, 0.0, 1.0],
            // v1 u32x3
            [1000.0, 1001.0, 1002.0, 1.0],
            // v1 u32x4
            [1000.0, 1001.0, 1002.0, 1003.0],
        ];

        for (i, exp) in expected.iter().enumerate() {
            let got = vec4(i);
            for lane in 0..4 {
                assert_f32_near(got[lane], exp[lane], &format!("vec4[{i}].{lane}"));
            }
        }
    });
}

#[test]
fn compute_can_vertex_pull_unorm8x2_and_unorm10_10_10_2_formats() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_can_vertex_pull_unorm8x2_and_unorm10_10_10_2_formats"
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

        const VB: u32 = 1;
        const META: u32 = 2;
        const OUT: u32 = 3;
        const SHADER: u32 = 4;
        const PIPELINE: u32 = 5;

        // Two vertices packed into one vertex buffer with an intentionally unaligned base offset.
        //
        // Vertex layout (stride=8 bytes):
        // - offset 0: DXGI_FORMAT_R8G8_UNORM (2 bytes)
        // - offset 2: padding (2 bytes)
        // - offset 4: DXGI_FORMAT_R10G10B10A2_UNORM (4 bytes)
        //
        // Base offset = 1 byte so both attributes exercise the unaligned byte-address stitching in
        // `ia_load_u32`.
        let mut vb = vec![
            0u8,   // base offset padding
            128u8, // v0 R8G8 = (128, 64)
            64u8, 0u8, 0u8,
        ];
        // v0 R10G10B10A2 = (r=1023, g=0, b=512, a=3)
        push_u32(&mut vb, 0xE000_03FF);

        vb.extend_from_slice(&[
            // v1 R8G8 = (0, 255)
            0u8, 255u8, 0u8, 0u8,
        ]);
        // v1 R10G10B10A2 = (r=0, g=1023, b=1023, a=0)
        push_u32(&mut vb, 0x3FFF_FC00);

        // Pad to a 4-byte multiple; the WGSL views the storage buffer as `array<u32>`.
        while !vb.len().is_multiple_of(4) {
            vb.push(0u8);
        }

        let mut meta = IaMeta::default();
        meta.vb[0].base_offset_bytes = 1;
        meta.vb[0].stride_bytes = 8;

        let output_vec4s = 2u32 * 2u32; // (r8g8, r10g10b10a2) per vertex
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
  let base = idx * 2u;

  let rg = ia_load_r8g8_unorm(0u, idx, 0u);
  let p = ia_load_r10g10b10a2_unorm(0u, idx, 4u);

  out_buf.data[base + 0u] = vec4<f32>(rg.x, rg.y, 0.0, 1.0);
  out_buf.data[base + 1u] = p;
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
        writer.create_buffer(VB, vb.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(META, meta.as_bytes().len() as u64, BufferUsage::UNIFORM);
        writer.create_buffer(
            OUT,
            output_size,
            BufferUsage::STORAGE | BufferUsage::MAP_READ,
        );

        writer.update_buffer(VB, 0, &vb);
        writer.update_buffer(META, 0, meta.as_bytes());

        writer.create_shader_module_wgsl(SHADER, &wgsl);
        writer.create_compute_pipeline(PIPELINE, SHADER, &bindings);

        writer.begin_compute_pass();
        writer.set_pipeline(PipelineKind::Compute, PIPELINE);
        writer.set_bind_buffer(IA_BINDING_META, META, 0, 0);
        for i in 0..IA_MAX_VERTEX_BUFFERS as u32 {
            writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + i, VB, 0, 0);
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
            // v0 r8g8
            [128.0 / 255.0, 64.0 / 255.0, 0.0, 1.0],
            // v0 r10g10b10a2
            [1.0, 0.0, 512.0 / 1023.0, 1.0],
            // v1 r8g8
            [0.0, 1.0, 0.0, 1.0],
            // v1 r10g10b10a2
            [0.0, 1.0, 1.0, 0.0],
        ];

        for (i, exp) in expected.iter().enumerate() {
            let got = vec4(i);
            for lane in 0..4 {
                assert_f32_near(got[lane], exp[lane], &format!("vec4[{i}].{lane}"));
            }
        }
    });
}

#[test]
fn compute_can_vertex_pull_unorm16_and_snorm16_formats() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_can_vertex_pull_unorm16_and_snorm16_formats"
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

        const VB: u32 = 1;
        const META: u32 = 2;
        const OUT: u32 = 3;
        const SHADER: u32 = 4;
        const PIPELINE: u32 = 5;

        // Two vertices packed into one vertex buffer with an intentionally unaligned base offset.
        //
        // Vertex layout (stride=32 bytes):
        // - offset 0:  DXGI_FORMAT_R16_UNORM (scalar)   [2 bytes]
        // - offset 2:  DXGI_FORMAT_R16_SNORM (scalar)   [2 bytes]
        // - offset 4:  DXGI_FORMAT_R16G16_UNORM         [4 bytes]
        // - offset 8:  DXGI_FORMAT_R16G16_SNORM         [4 bytes]
        // - offset 12: DXGI_FORMAT_R16G16B16A16_UNORM   [8 bytes]
        // - offset 20: DXGI_FORMAT_R16G16B16A16_SNORM   [8 bytes]
        //
        // Base offset = 1 byte so the shader must stitch unaligned `u32` reads.
        let mut vb = Vec::<u8>::new();
        vb.push(0u8); // base offset padding

        // v0
        push_u16(&mut vb, 0); // r16_unorm = 0.0
        push_u16(&mut vb, 0x8000); // r16_snorm = -1.0
        push_u16(&mut vb, 0); // r16g16_unorm.x = 0.0
        push_u16(&mut vb, 0xFFFF); // r16g16_unorm.y = 1.0
        push_u16(&mut vb, 0x8000); // r16g16_snorm.x = -1.0
        push_u16(&mut vb, 0x7FFF); // r16g16_snorm.y = 1.0
                                   // r16g16b16a16_unorm = (1,0,1,0)
        push_u16(&mut vb, 0xFFFF);
        push_u16(&mut vb, 0);
        push_u16(&mut vb, 0xFFFF);
        push_u16(&mut vb, 0);
        // r16g16b16a16_snorm = (-1,0,1,-1)
        push_u16(&mut vb, 0x8000);
        push_u16(&mut vb, 0);
        push_u16(&mut vb, 0x7FFF);
        push_u16(&mut vb, 0x8000);
        // padding to 32 bytes
        vb.extend_from_slice(&[0u8; 4]);

        // v1
        push_u16(&mut vb, 0xFFFF); // r16_unorm = 1.0
        push_u16(&mut vb, 0x7FFF); // r16_snorm = 1.0
        push_u16(&mut vb, 0xFFFF); // r16g16_unorm.x = 1.0
        push_u16(&mut vb, 0); // r16g16_unorm.y = 0.0
        push_u16(&mut vb, 0x7FFF); // r16g16_snorm.x = 1.0
        push_u16(&mut vb, 0x8000); // r16g16_snorm.y = -1.0
                                   // r16g16b16a16_unorm = (0,1,0,1)
        push_u16(&mut vb, 0);
        push_u16(&mut vb, 0xFFFF);
        push_u16(&mut vb, 0);
        push_u16(&mut vb, 0xFFFF);
        // r16g16b16a16_snorm = (0,-1,0,1)
        push_u16(&mut vb, 0);
        push_u16(&mut vb, 0x8000);
        push_u16(&mut vb, 0);
        push_u16(&mut vb, 0x7FFF);
        // padding to 32 bytes
        vb.extend_from_slice(&[0u8; 4]);

        assert_eq!(vb.len(), 1 + 32 * 2);
        while !vb.len().is_multiple_of(4) {
            vb.push(0);
        }

        let mut meta = IaMeta::default();
        meta.vb[0].base_offset_bytes = 1;
        meta.vb[0].stride_bytes = 32;

        let output_vec4s = 2u32 * 6u32;
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
  let base = idx * 6u;

  let u0 = ia_load_r16_unorm(0u, idx, 0u);
  let s0 = ia_load_r16_snorm(0u, idx, 2u);
  let u2 = ia_load_r16g16_unorm(0u, idx, 4u);
  let s2 = ia_load_r16g16_snorm(0u, idx, 8u);
  let u4 = ia_load_r16g16b16a16_unorm(0u, idx, 12u);
  let s4 = ia_load_r16g16b16a16_snorm(0u, idx, 20u);

  out_buf.data[base + 0u] = vec4<f32>(u0, 0.0, 0.0, 1.0);
  out_buf.data[base + 1u] = vec4<f32>(s0, 0.0, 0.0, 1.0);
  out_buf.data[base + 2u] = vec4<f32>(u2.x, u2.y, 0.0, 1.0);
  out_buf.data[base + 3u] = vec4<f32>(s2.x, s2.y, 0.0, 1.0);
  out_buf.data[base + 4u] = u4;
  out_buf.data[base + 5u] = s4;
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
        writer.create_buffer(VB, vb.len() as u64, BufferUsage::VERTEX);
        writer.create_buffer(META, meta.as_bytes().len() as u64, BufferUsage::UNIFORM);
        writer.create_buffer(
            OUT,
            output_size,
            BufferUsage::STORAGE | BufferUsage::MAP_READ,
        );

        writer.update_buffer(VB, 0, &vb);
        writer.update_buffer(META, 0, meta.as_bytes());

        writer.create_shader_module_wgsl(SHADER, &wgsl);
        writer.create_compute_pipeline(PIPELINE, SHADER, &bindings);

        writer.begin_compute_pass();
        writer.set_pipeline(PipelineKind::Compute, PIPELINE);
        writer.set_bind_buffer(IA_BINDING_META, META, 0, 0);
        for i in 0..IA_MAX_VERTEX_BUFFERS as u32 {
            writer.set_bind_buffer(IA_BINDING_VERTEX_BUFFER_BASE + i, VB, 0, 0);
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
            // v0
            [0.0, 0.0, 0.0, 1.0],   // r16_unorm
            [-1.0, 0.0, 0.0, 1.0],  // r16_snorm
            [0.0, 1.0, 0.0, 1.0],   // r16g16_unorm
            [-1.0, 1.0, 0.0, 1.0],  // r16g16_snorm
            [1.0, 0.0, 1.0, 0.0],   // r16g16b16a16_unorm
            [-1.0, 0.0, 1.0, -1.0], // r16g16b16a16_snorm
            // v1
            [1.0, 0.0, 0.0, 1.0],  // r16_unorm
            [1.0, 0.0, 0.0, 1.0],  // r16_snorm
            [1.0, 0.0, 0.0, 1.0],  // r16g16_unorm
            [1.0, -1.0, 0.0, 1.0], // r16g16_snorm
            [0.0, 1.0, 0.0, 1.0],  // r16g16b16a16_unorm
            [0.0, -1.0, 0.0, 1.0], // r16g16b16a16_snorm
        ];

        for (i, exp) in expected.iter().enumerate() {
            let got = vec4(i);
            for lane in 0..4 {
                assert_f32_near(got[lane], exp[lane], &format!("vec4[{i}].{lane}"));
            }
        }
    });
}
