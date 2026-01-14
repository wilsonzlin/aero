mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundBuffer, BoundConstantBuffer, BoundSampler, BoundTexture, ShaderStage};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuConstantBufferBinding, AerogpuSamplerAddressMode, AerogpuSamplerFilter,
    AerogpuShaderResourceBufferBinding, AerogpuShaderStage, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_STORAGE, AEROGPU_RESOURCE_USAGE_TEXTURE,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn aerogpu_cmd_geometry_stage_bindings_do_not_clobber_compute() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut writer = AerogpuCmdWriter::new();

        // Create minimal resources used for bindings.
        // Textures.
        writer.create_texture2d(
            303,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            0,
            0,
            0,
        );
        writer.create_texture2d(
            304,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            0,
            0,
            0,
        );

        // Samplers.
        writer.create_sampler(
            203,
            AerogpuSamplerFilter::Nearest,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
        );
        writer.create_sampler(
            204,
            AerogpuSamplerFilter::Nearest,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
        );

        // Constant buffers.
        writer.create_buffer(103, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, 64, 0, 0);
        writer.create_buffer(104, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, 64, 0, 0);

        // SRV buffers (t# buffer bindings). Use a separate handle range to avoid confusion with
        // constant buffers.
        writer.create_buffer(503, AEROGPU_RESOURCE_USAGE_STORAGE, 64, 0, 0);
        writer.create_buffer(504, AEROGPU_RESOURCE_USAGE_STORAGE, 64, 0, 0);

        // Compute stage baseline bindings.
        writer.set_constant_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuConstantBufferBinding {
                buffer: 103,
                offset_bytes: 0,
                size_bytes: 16,
                reserved0: 0,
            }],
        );
        writer.set_samplers(AerogpuShaderStage::Compute, 0, &[203]);
        writer.set_texture(AerogpuShaderStage::Compute, 0, 303);
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Compute,
            1,
            &[AerogpuShaderResourceBufferBinding {
                buffer: 503,
                offset_bytes: 0,
                size_bytes: 0, // 0 = whole buffer
                reserved0: 0,
            }],
        );

        // Geometry stage bindings using the direct `shader_stage = GEOMETRY` encoding.
        // These must not overwrite the compute stage bindings above.
        writer.set_constant_buffers(
            AerogpuShaderStage::Geometry,
            0,
            &[AerogpuConstantBufferBinding {
                buffer: 104,
                offset_bytes: 0,
                size_bytes: 16,
                reserved0: 0,
            }],
        );
        writer.set_samplers(AerogpuShaderStage::Geometry, 0, &[204]);
        writer.set_texture(AerogpuShaderStage::Geometry, 0, 304);
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Geometry,
            1,
            &[AerogpuShaderResourceBufferBinding {
                buffer: 504,
                offset_bytes: 0,
                size_bytes: 0, // 0 = whole buffer
                reserved0: 0,
            }],
        );

        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        let bindings = exec.binding_state();

        let expect_cb = |buffer: u32| {
            Some(BoundConstantBuffer {
                buffer,
                offset: 0,
                size: Some(16),
            })
        };

        assert_eq!(
            bindings.stage(ShaderStage::Compute).constant_buffer(0),
            expect_cb(103)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).sampler(0),
            Some(BoundSampler { sampler: 203 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).texture(0),
            Some(BoundTexture { texture: 303 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).srv_buffer(1),
            Some(BoundBuffer {
                buffer: 503,
                offset: 0,
                size: None,
            })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Geometry).constant_buffer(0),
            expect_cb(104)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).sampler(0),
            Some(BoundSampler { sampler: 204 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).texture(0),
            Some(BoundTexture { texture: 304 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).srv_buffer(1),
            Some(BoundBuffer {
                buffer: 504,
                offset: 0,
                size: None,
            })
        );
    });
}
