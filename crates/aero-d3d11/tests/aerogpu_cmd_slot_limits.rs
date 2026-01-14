mod common;

use aero_d3d11::binding_model::D3D11_MAX_CONSTANT_BUFFER_SLOTS;
use aero_d3d11::binding_model::{MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS, MAX_UAV_SLOTS};
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

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

#[test]
fn aerogpu_cmd_set_samplers_rejects_slot_out_of_range() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetSamplers as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&MAX_SAMPLER_SLOTS.to_le_bytes()); // start_slot (0..MAX_SAMPLER_SLOTS-1 supported)
        stream.extend_from_slice(&1u32.to_le_bytes()); // sampler_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // sampler handle (unbind)
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);
        let mut guest_mem = VecGuestMemory::new(0x1000);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("expected SET_SAMPLERS to reject out-of-range slot");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SET_SAMPLERS: slot range out of supported range"),
            "{msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_set_constant_buffers_rejects_slot_out_of_range() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetConstantBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // shader_stage = vertex
                                                       // start_slot (0..D3D11_MAX_CONSTANT_BUFFER_SLOTS-1 supported)
        stream.extend_from_slice(&D3D11_MAX_CONSTANT_BUFFER_SLOTS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                       // aerogpu_constant_buffer_binding (16 bytes)
        stream.extend_from_slice(&0u32.to_le_bytes()); // buffer handle (unbind)
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);
        let mut guest_mem = VecGuestMemory::new(0x1000);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("expected SET_CONSTANT_BUFFERS to reject out-of-range slot");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SET_CONSTANT_BUFFERS: slot range out of supported range"),
            "{msg}"
        );
    });
}
#[test]
fn aerogpu_cmd_set_texture_rejects_slot_out_of_range() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&MAX_TEXTURE_SLOTS.to_le_bytes()); // slot (0..MAX_TEXTURE_SLOTS-1 supported)
        stream.extend_from_slice(&0u32.to_le_bytes()); // texture handle (unbind)
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);
        let mut guest_mem = VecGuestMemory::new(0x1000);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("expected SET_TEXTURE to reject out-of-range slot");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SET_TEXTURE: slot out of supported range"),
            "{msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_set_shader_resource_buffers_rejects_slot_out_of_range() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(
            &mut stream,
            AerogpuCmdOpcode::SetShaderResourceBuffers as u32,
        );
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
                                                       // start_slot (0..MAX_TEXTURE_SLOTS-1 supported)
        stream.extend_from_slice(&MAX_TEXTURE_SLOTS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                       // aerogpu_shader_resource_buffer_binding (16 bytes)
        stream.extend_from_slice(&0u32.to_le_bytes()); // buffer handle (unbind)
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);
        let mut guest_mem = VecGuestMemory::new(0x1000);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("expected SET_SHADER_RESOURCE_BUFFERS to reject out-of-range slot");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SET_SHADER_RESOURCE_BUFFERS: slot range out of supported range"),
            "{msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_set_unordered_access_buffers_rejects_slot_out_of_range() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(
            &mut stream,
            AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32,
        );
        stream.extend_from_slice(&2u32.to_le_bytes()); // shader_stage = compute
                                                       // start_slot (0..MAX_UAV_SLOTS-1 supported)
        stream.extend_from_slice(&MAX_UAV_SLOTS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // uav_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                       // aerogpu_unordered_access_buffer_binding (16 bytes)
        stream.extend_from_slice(&0u32.to_le_bytes()); // buffer handle (unbind)
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // initial_count
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);
        let mut guest_mem = VecGuestMemory::new(0x1000);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("expected SET_UNORDERED_ACCESS_BUFFERS to reject out-of-range slot");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SET_UNORDERED_ACCESS_BUFFERS: slot range out of supported range"),
            "{msg}"
        );
    });
}
