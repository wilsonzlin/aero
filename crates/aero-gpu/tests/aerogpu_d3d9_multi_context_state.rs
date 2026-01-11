mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
        AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn align4(v: usize) -> usize {
    (v + 3) & !3
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);
    let end_aligned = align4(out.len());
    out.resize(end_aligned, 0);
    let size_bytes = (end_aligned - start) as u32;
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

#[test]
fn d3d9_cmd_stream_isolates_state_per_context() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

    const RT1_HANDLE: u32 = 1;
    const RT2_HANDLE: u32 = 2;

    const CTX_A: u32 = 100;
    const CTX_B: u32 = 200;

    let width = 4u32;
    let height = 4u32;

    let stream_a1 = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, RT1_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT1_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // Clear red.
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream_for_context(CTX_A, &stream_a1)
        .expect("context A init should succeed");

    let stream_b1 = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, RT2_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT2_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // Clear green.
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream_for_context(CTX_B, &stream_b1)
        .expect("context B init should succeed");

    // Context A clears again without re-binding render targets. If the executor incorrectly shares
    // render-target state across contexts, this clear would apply to RT2 instead of RT1.
    let stream_a2 = build_stream(|out| {
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // Clear blue.
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream_for_context(CTX_A, &stream_a2)
        .expect("context A second clear should succeed");

    let (_, _, rgba1) = pollster::block_on(exec.readback_texture_rgba8(RT1_HANDLE))
        .expect("readback RT1 should succeed");
    let (_, _, rgba2) = pollster::block_on(exec.readback_texture_rgba8(RT2_HANDLE))
        .expect("readback RT2 should succeed");

    assert_eq!(&rgba1[0..4], &[0, 0, 255, 255], "RT1 should be blue");
    assert_eq!(&rgba2[0..4], &[0, 255, 0, 255], "RT2 should remain green");
}
