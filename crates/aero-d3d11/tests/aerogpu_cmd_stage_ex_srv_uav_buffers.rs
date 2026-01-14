mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundBuffer, BoundTexture, ShaderStage};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuShaderStage,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

// `stage_ex` values use DXBC program-type numbering (SM4/SM5 version token).
const STAGE_EX_HULL: u32 = 3;
const STAGE_EX_DOMAIN: u32 = 4;

fn begin_cmd(stream: &mut Vec<u8>, opcode: u32) -> usize {
    let start = stream.len();
    stream.extend_from_slice(&opcode.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size placeholder
    start
}

fn end_cmd(stream: &mut [u8], start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

fn finish_stream(mut stream: Vec<u8>) -> Vec<u8> {
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
    stream
}

fn push_create_buffer(stream: &mut Vec<u8>, handle: u32, size: u64) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&handle.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes()); // usage_flags
    stream.extend_from_slice(&size.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(stream, start);
}

fn push_export_shared_surface(stream: &mut Vec<u8>, handle: u32, token: u64) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
    stream.extend_from_slice(&handle.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&token.to_le_bytes());
    end_cmd(stream, start);
}

fn push_import_shared_surface(stream: &mut Vec<u8>, out_handle: u32, token: u64) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
    stream.extend_from_slice(&out_handle.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&token.to_le_bytes());
    end_cmd(stream, start);
}

fn push_set_texture(stream: &mut Vec<u8>, stage: u32, slot: u32, texture: u32, stage_ex: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetTexture as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes());
    stream.extend_from_slice(&texture.to_le_bytes());
    stream.extend_from_slice(&stage_ex.to_le_bytes());
    end_cmd(stream, start);
}

fn push_set_shader_resource_buffer(
    stream: &mut Vec<u8>,
    stage: u32,
    slot: u32,
    stage_ex: u32,
    buffer: u32,
    offset: u32,
    size: u32,
) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&stage_ex.to_le_bytes());

    // struct aerogpu_shader_resource_buffer_binding
    stream.extend_from_slice(&buffer.to_le_bytes());
    stream.extend_from_slice(&offset.to_le_bytes());
    stream.extend_from_slice(&size.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(stream, start);
}

fn push_set_shader_resource_buffers_2(
    stream: &mut Vec<u8>,
    stage: u32,
    start_slot: u32,
    stage_ex: u32,
    binding0: (u32, u32, u32),
    binding1: (u32, u32, u32),
) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&start_slot.to_le_bytes());
    stream.extend_from_slice(&2u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&stage_ex.to_le_bytes());

    for (buffer, offset, size) in [binding0, binding1] {
        stream.extend_from_slice(&buffer.to_le_bytes());
        stream.extend_from_slice(&offset.to_le_bytes());
        stream.extend_from_slice(&size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    }
    end_cmd(stream, start);
}

#[allow(clippy::too_many_arguments)]
fn push_set_uav_buffer(
    stream: &mut Vec<u8>,
    stage: u32,
    slot: u32,
    stage_ex: u32,
    buffer: u32,
    offset: u32,
    size: u32,
    initial_count: u32,
) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // uav_count
    stream.extend_from_slice(&stage_ex.to_le_bytes());

    // struct aerogpu_unordered_access_buffer_binding
    stream.extend_from_slice(&buffer.to_le_bytes());
    stream.extend_from_slice(&offset.to_le_bytes());
    stream.extend_from_slice(&size.to_le_bytes());
    stream.extend_from_slice(&initial_count.to_le_bytes());
    end_cmd(stream, start);
}

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

        let mut stream = vec![0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
        stream[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());

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
            push_create_buffer(&mut stream, handle, 256);
        }

        // Create shared-surface aliases for SRV + UAV buffers so the binding commands must resolve
        // alias handles to the underlying resource.
        push_export_shared_surface(&mut stream, BUF_SRV_UNDERLYING, TOKEN_SRV);
        push_import_shared_surface(&mut stream, BUF_SRV_ALIAS, TOKEN_SRV);

        push_export_shared_surface(&mut stream, BUF_UAV_UNDERLYING, TOKEN_UAV);
        push_import_shared_surface(&mut stream, BUF_UAV_ALIAS, TOKEN_UAV);

        // Mutual exclusion: setting an SRV buffer must unbind a texture at the same `t#` slot.
        push_set_texture(&mut stream, AerogpuShaderStage::Vertex as u32, 0, TEX_VS, 0);
        push_set_shader_resource_buffer(
            &mut stream,
            AerogpuShaderStage::Vertex as u32,
            0,
            0,
            BUF_SRV_ALIAS,
            16,
            64,
        );

        // Pixel stage SRV buffers: slot 1 will be overridden by SET_TEXTURE to validate the other
        // direction of mutual exclusion; slot 2 should remain bound as an SRV buffer.
        push_set_shader_resource_buffers_2(
            &mut stream,
            AerogpuShaderStage::Pixel as u32,
            1,
            0,
            (BUF_PS_SRV1, 0, 0),
            (BUF_PS_SRV2, 4, 0),
        );
        // Mutual exclusion: setting a texture must unbind any SRV buffer at the same `t#` slot.
        push_set_texture(&mut stream, AerogpuShaderStage::Pixel as u32, 1, TEX_PS, 0);

        // Compute stage bindings (legacy CS).
        push_set_shader_resource_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            0,
            BUF_CS_SRV1,
            0,
            0,
        );
        push_set_uav_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            0,
            BUF_CS_UAV1,
            0,
            0,
            0,
        );

        // Stage-ex HS/DS bindings use shader_stage==COMPUTE + reserved0 stage_ex to route into the
        // correct per-stage binding table (HS/DS compute-emulation path).
        push_set_shader_resource_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            STAGE_EX_HULL,
            BUF_HS_SRV,
            8,
            16,
        );
        push_set_uav_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            STAGE_EX_HULL,
            BUF_UAV_ALIAS,
            0,
            0,
            7, // initial_count ignored by executor (not yet implemented)
        );
        push_set_shader_resource_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            STAGE_EX_DOMAIN,
            BUF_DS_SRV,
            0,
            0,
        );
        push_set_uav_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            STAGE_EX_DOMAIN,
            BUF_DS_UAV,
            0,
            0,
            0,
        );

        // Second CS update ensures compute and stage_ex buckets remain distinct in both directions.
        push_set_shader_resource_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            0,
            BUF_CS_SRV2,
            0,
            0,
        );
        push_set_uav_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            0,
            BUF_CS_UAV2,
            0,
            0,
            0,
        );

        let stream = finish_stream(stream);

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
