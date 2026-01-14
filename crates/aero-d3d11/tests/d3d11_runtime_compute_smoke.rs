mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, PipelineKind, ShaderStageFlags,
};

#[test]
fn d3d11_runtime_compute_smoke() {
    pollster::block_on(async {
        let test_name = concat!(module_path!(), "::d3d11_runtime_compute_smoke");

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

        const SHADER: u32 = 1;
        const PIPE: u32 = 2;
        const BUF: u32 = 3;
        const STAGING: u32 = 4;

        let wgsl = r#"
@group(2) @binding(0) var<storage, read_write> out: array<u32>;

@compute @workgroup_size(1)
fn cs_main() {
    out[0] = 0x12345678u;
}
"#;

        let bindings = [BindingDesc {
            binding: 0,
            ty: BindingType::StorageBufferReadWrite,
            visibility: ShaderStageFlags::COMPUTE,
            storage_texture_format: None,
        }];

        let mut w = CmdWriter::new();
        w.create_shader_module_wgsl(SHADER, wgsl);
        w.create_buffer(
            BUF,
            4,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        );
        w.update_buffer(BUF, 0, &0u32.to_le_bytes());
        w.create_buffer(STAGING, 4, BufferUsage::MAP_READ | BufferUsage::COPY_DST);
        w.create_compute_pipeline(PIPE, SHADER, &bindings);

        w.begin_compute_pass();
        w.set_pipeline(PipelineKind::Compute, PIPE);
        w.set_bind_buffer(0, BUF, 0, 4);
        w.dispatch(1, 1, 1);
        w.end_compute_pass();

        // Copy the results into a MAP_READ buffer for portable readback.
        w.copy_buffer_to_buffer(BUF, 0, STAGING, 0, 4);

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let got = rt.read_buffer(STAGING, 0, 4).await.unwrap();
        let got = u32::from_le_bytes(got[..4].try_into().unwrap());
        assert_eq!(got, 0x12345678);
    });
}
