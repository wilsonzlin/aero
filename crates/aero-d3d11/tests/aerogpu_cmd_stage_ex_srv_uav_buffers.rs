mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundBuffer, BoundTexture, ShaderStage};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuShaderResourceBufferBinding, AerogpuShaderStage, AerogpuShaderStageEx,
    AerogpuUnorderedAccessBufferBinding, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn aerogpu_cmd_stage_ex_srv_uav_buffers_route_and_unbind_correctly() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const BUF_SRV_UNDERLYING: u32 = 100;
        const BUF_SRV_ALIAS: u32 = 101;
        const BUF_PS_SRV1: u32 = 110;
        const BUF_PS_SRV2: u32 = 111;

        const BUF_CS_SRV1: u32 = 120;
        const BUF_CS_SRV2: u32 = 121;
        const BUF_HS_SRV: u32 = 130;
        const BUF_DS_SRV: u32 = 131;

        const BUF_UAV_UNDERLYING: u32 = 200;
        const BUF_UAV_ALIAS: u32 = 201;
        const BUF_CS_UAV1: u32 = 210;
        const BUF_CS_UAV2: u32 = 211;
        const BUF_DS_UAV: u32 = 220;

        const TOKEN_SRV: u64 = 0x0123_4567_89AB_CDEF;
        const TOKEN_UAV: u64 = 0x0FED_CBA9_7654_3210;

        const TEX_VS: u32 = 900;
        const TEX_PS: u32 = 901;

        let mut writer = AerogpuCmdWriter::new();

        // Create all underlying buffers.
        for handle in [
            BUF_SRV_UNDERLYING,
            BUF_PS_SRV1,
            BUF_PS_SRV2,
            BUF_CS_SRV1,
            BUF_CS_SRV2,
            BUF_HS_SRV,
            BUF_DS_SRV,
            BUF_UAV_UNDERLYING,
            BUF_CS_UAV1,
            BUF_CS_UAV2,
            BUF_DS_UAV,
        ] {
            writer.create_buffer(handle, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 256, 0, 0);
        }

        // Create shared-surface aliases for SRV + UAV buffers so the binding commands must resolve
        // alias handles to the underlying resource.
        writer.export_shared_surface(BUF_SRV_UNDERLYING, TOKEN_SRV);
        writer.import_shared_surface(BUF_SRV_ALIAS, TOKEN_SRV);

        writer.export_shared_surface(BUF_UAV_UNDERLYING, TOKEN_UAV);
        writer.import_shared_surface(BUF_UAV_ALIAS, TOKEN_UAV);

        // Mutual exclusion: setting an SRV buffer must unbind a texture at the same `t#` slot.
        writer.set_texture(AerogpuShaderStage::Vertex, 0, TEX_VS);
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Vertex,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: BUF_SRV_ALIAS,
                offset_bytes: 16,
                size_bytes: 64,
                reserved0: 0,
            }],
        );

        // Pixel stage SRV buffers: slot 1 will be overridden by SET_TEXTURE to validate the other
        // direction of mutual exclusion; slot 2 should remain bound as an SRV buffer.
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Pixel,
            1,
            &[
                AerogpuShaderResourceBufferBinding {
                    buffer: BUF_PS_SRV1,
                    offset_bytes: 0,
                    size_bytes: 0,
                    reserved0: 0,
                },
                AerogpuShaderResourceBufferBinding {
                    buffer: BUF_PS_SRV2,
                    offset_bytes: 4,
                    size_bytes: 0,
                    reserved0: 0,
                },
            ],
        );
        // Mutual exclusion: setting a texture must unbind any SRV buffer at the same `t#` slot.
        writer.set_texture(AerogpuShaderStage::Pixel, 1, TEX_PS);

        // Compute stage bindings (legacy CS).
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: BUF_CS_SRV1,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_unordered_access_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: BUF_CS_UAV1,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );

        // Stage-ex HS/DS bindings use shader_stage==COMPUTE + reserved0 stage_ex to route into the
        // correct per-stage binding table (HS/DS compute-emulation path).
        writer.set_shader_resource_buffers_ex(
            AerogpuShaderStageEx::Hull,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: BUF_HS_SRV,
                offset_bytes: 8,
                size_bytes: 16,
                reserved0: 0,
            }],
        );
        writer.set_unordered_access_buffers_ex(
            AerogpuShaderStageEx::Hull,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: BUF_UAV_ALIAS,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 7, // ignored by executor (not yet implemented)
            }],
        );
        writer.set_shader_resource_buffers_ex(
            AerogpuShaderStageEx::Domain,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: BUF_DS_SRV,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_unordered_access_buffers_ex(
            AerogpuShaderStageEx::Domain,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: BUF_DS_UAV,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );

        // Second CS update ensures compute and stage_ex buckets remain distinct in both directions.
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: BUF_CS_SRV2,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_unordered_access_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: BUF_CS_UAV2,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );

        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        let bindings = exec.binding_state();

        assert_eq!(
            bindings.stage(ShaderStage::Vertex).texture(0),
            None,
            "binding an SRV buffer must unbind an existing texture binding at the same slot"
        );
        assert_eq!(
            bindings.stage(ShaderStage::Vertex).srv_buffer(0),
            Some(BoundBuffer {
                buffer: BUF_SRV_UNDERLYING,
                offset: 16,
                size: Some(64),
            }),
            "SRV buffer binding must resolve shared-surface alias handles"
        );

        assert_eq!(
            bindings.stage(ShaderStage::Pixel).texture(1),
            Some(BoundTexture { texture: TEX_PS }),
        );
        assert_eq!(
            bindings.stage(ShaderStage::Pixel).srv_buffer(1),
            None,
            "binding a texture must unbind any existing SRV buffer binding at the same slot"
        );
        assert_eq!(
            bindings.stage(ShaderStage::Pixel).srv_buffer(2),
            Some(BoundBuffer {
                buffer: BUF_PS_SRV2,
                offset: 4,
                size: None,
            }),
        );

        assert_eq!(
            bindings.stage(ShaderStage::Compute).srv_buffer(0),
            Some(BoundBuffer {
                buffer: BUF_CS_SRV2,
                offset: 0,
                size: None,
            }),
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).srv_buffer(0),
            Some(BoundBuffer {
                buffer: BUF_HS_SRV,
                offset: 8,
                size: Some(16),
            }),
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).srv_buffer(0),
            Some(BoundBuffer {
                buffer: BUF_DS_SRV,
                offset: 0,
                size: None,
            }),
        );

        assert_eq!(
            bindings.stage(ShaderStage::Compute).uav_buffer(0),
            Some(BoundBuffer {
                buffer: BUF_CS_UAV2,
                offset: 0,
                size: None,
            }),
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).uav_buffer(0),
            Some(BoundBuffer {
                buffer: BUF_UAV_UNDERLYING,
                offset: 0,
                size: None,
            }),
            "UAV buffer binding must resolve shared-surface alias handles"
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).uav_buffer(0),
            Some(BoundBuffer {
                buffer: BUF_DS_UAV,
                offset: 0,
                size: None,
            }),
        );
    });
}
