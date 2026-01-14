mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, PipelineKind, ShaderStageFlags,
};

#[test]
fn d3d11_runtime_compute_dispatch_writes_storage_buffer() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_compute_dispatch_writes_storage_buffer"
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

        const STORAGE_BUF: u32 = 1;
        const READBACK_BUF: u32 = 2;
        const SHADER: u32 = 3;
        const PIPELINE: u32 = 4;

        const ELEMENTS: u32 = 16;
        let size = (ELEMENTS as u64) * 4;

        let initial_words = vec![0xDEADBEEFu32; ELEMENTS as usize];

        let wgsl = r#"
@group(2) @binding(0)
var<storage, read_write> buf: array<u32>;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    buf[id.x] = id.x;
}
"#;

        let mut w = CmdWriter::new();
        w.create_buffer(
            STORAGE_BUF,
            size,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        );
        w.create_buffer(
            READBACK_BUF,
            size,
            BufferUsage::MAP_READ | BufferUsage::COPY_DST,
        );
        w.update_buffer(STORAGE_BUF, 0, bytemuck::cast_slice(&initial_words));
        w.create_shader_module_wgsl(SHADER, wgsl);
        w.create_compute_pipeline(
            PIPELINE,
            SHADER,
            &[BindingDesc {
                binding: 0,
                ty: BindingType::StorageBufferReadWrite,
                visibility: ShaderStageFlags::COMPUTE,
                storage_texture_format: None,
            }],
        );
        w.set_pipeline(PipelineKind::Compute, PIPELINE);
        w.set_bind_buffer(0, STORAGE_BUF, 0, size);

        w.begin_compute_pass();
        w.dispatch(ELEMENTS, 1, 1);
        w.end_compute_pass();

        // Map-read buffers are generally "staging" resources; copy the results into a dedicated
        // readback buffer so this test works on backends that don't support mapping a storage
        // buffer directly.
        w.copy_buffer_to_buffer(STORAGE_BUF, 0, READBACK_BUF, 0, size);

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let got = rt.read_buffer(READBACK_BUF, 0, size).await.unwrap();

        let mut expected = Vec::with_capacity(size as usize);
        for i in 0..ELEMENTS {
            expected.extend_from_slice(&i.to_le_bytes());
        }
        assert_eq!(got, expected);
    });
}

#[test]
fn d3d11_runtime_vertex_buffer_is_bindable_as_storage_for_vertex_pulling() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_vertex_buffer_is_bindable_as_storage_for_vertex_pulling"
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
        const SHADER: u32 = 2;
        const PIPELINE: u32 = 3;

        // Any non-zero size works; keep it 16 bytes to satisfy any backend alignment rules.
        let size = 16u64;

        let wgsl = r#"
@group(0) @binding(0)
var<storage, read> buf: array<u32>;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let _x: u32 = buf[0];
}
"#;

        let mut w = CmdWriter::new();
        w.create_buffer(VB, size, BufferUsage::VERTEX | BufferUsage::COPY_DST);
        w.update_buffer(VB, 0, bytemuck::cast_slice(&[0x1234_5678u32, 0, 0, 0]));
        w.create_shader_module_wgsl(SHADER, wgsl);
        w.create_compute_pipeline(
            PIPELINE,
            SHADER,
            &[BindingDesc {
                binding: 0,
                ty: BindingType::StorageBufferReadOnly,
                visibility: ShaderStageFlags::COMPUTE,
                storage_texture_format: None,
            }],
        );
        w.set_pipeline(PipelineKind::Compute, PIPELINE);
        w.set_bind_buffer(0, VB, 0, size);
        w.begin_compute_pass();
        w.dispatch(1, 1, 1);
        w.end_compute_pass();

        // Validation check: binding a vertex buffer as `var<storage>` must not trigger a wgpu
        // validation error (vertex pulling compute prepasses rely on this).
        rt.device().push_error_scope(wgpu::ErrorFilter::Validation);
        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();
        let err = rt.device().pop_error_scope().await;
        assert!(
            err.is_none(),
            "vertex buffers must be created with STORAGE for vertex pulling, got: {err:?}"
        );
    });
}
