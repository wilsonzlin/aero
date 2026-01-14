mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundBuffer, BoundTexture, ShaderStage};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuShaderStage,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);
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

fn new_stream() -> Vec<u8> {
    let mut stream = vec![0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
    stream[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    stream
}

fn push_set_texture(stream: &mut Vec<u8>, stage: u32, slot: u32, texture: u32, stage_ex: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetTexture as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes());
    stream.extend_from_slice(&texture.to_le_bytes());
    stream.extend_from_slice(&stage_ex.to_le_bytes());
    end_cmd(stream, start);
}

fn push_set_shader_resource_buffer(stream: &mut Vec<u8>, stage: u32, slot: u32, buffer: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0 / stage_ex
                                                   // struct aerogpu_shader_resource_buffer_binding
    stream.extend_from_slice(&buffer.to_le_bytes()); // buffer handle (0 = unbind)
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full buffer)
    stream.extend_from_slice(&0u32.to_le_bytes()); // binding reserved0
    end_cmd(stream, start);
}

fn push_set_unordered_access_buffer(stream: &mut Vec<u8>, stage: u32, slot: u32, buffer: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // uav_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0 / stage_ex

    // struct aerogpu_unordered_access_buffer_binding
    stream.extend_from_slice(&buffer.to_le_bytes()); // buffer handle
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full buffer)
    stream.extend_from_slice(&0u32.to_le_bytes()); // initial_count

    end_cmd(stream, start);
}

#[test]
fn aerogpu_cmd_uav_buffers_do_not_clobber_compute_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut guest_mem = VecGuestMemory::new(0);

        const TEX: u32 = 1000;
        const UAV_BUF: u32 = 2000;

        // Bind a texture in compute stage at `t0`, then bind a UAV buffer at `u0`. UAV binding
        // should not clobber `t0` because the register spaces are distinct.
        {
            let mut stream = new_stream();
            push_set_texture(&mut stream, AerogpuShaderStage::Compute as u32, 0, TEX, 0);
            push_set_unordered_access_buffer(
                &mut stream,
                AerogpuShaderStage::Compute as u32,
                0,
                UAV_BUF,
            );
            let stream = finish_stream(stream);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("command stream should execute");

            let bindings = exec.binding_state();
            assert_eq!(
                bindings.stage(ShaderStage::Compute).texture(0),
                Some(BoundTexture { texture: TEX })
            );
            assert_eq!(
                bindings.stage(ShaderStage::Compute).uav_buffer(0),
                Some(BoundBuffer {
                    buffer: UAV_BUF,
                    offset: 0,
                    size: None,
                })
            );
        }

        // Unbind the UAV buffer; the SRV texture should remain bound.
        {
            let mut stream = new_stream();
            push_set_unordered_access_buffer(&mut stream, AerogpuShaderStage::Compute as u32, 0, 0);
            let stream = finish_stream(stream);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("command stream should execute");

            let bindings = exec.binding_state();
            assert_eq!(
                bindings.stage(ShaderStage::Compute).texture(0),
                Some(BoundTexture { texture: TEX })
            );
            assert_eq!(bindings.stage(ShaderStage::Compute).uav_buffer(0), None);
        }
    });
}

#[test]
fn aerogpu_cmd_srv_buffer_unbind_clears_binding() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut guest_mem = VecGuestMemory::new(0);

        const SRV_BUF: u32 = 1234;

        let mut stream = new_stream();
        push_set_shader_resource_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            SRV_BUF,
        );
        push_set_shader_resource_buffer(&mut stream, AerogpuShaderStage::Compute as u32, 0, 0);
        let stream = finish_stream(stream);

        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        let bindings = exec.binding_state();
        assert_eq!(bindings.stage(ShaderStage::Compute).srv_buffer(0), None);
    });
}
