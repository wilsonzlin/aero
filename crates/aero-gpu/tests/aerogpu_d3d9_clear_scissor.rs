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

// D3D9 render state IDs (subset).
const D3DRS_SCISSORTESTENABLE: u32 = 174;

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_i32(out: &mut Vec<u8>, v: i32) {
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

fn pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
}

#[test]
fn d3d9_cmd_stream_clear_respects_scissor_rect() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;

    let width = 64u32;
    let height = 64u32;

    let scissor_x = 8i32;
    let scissor_y = 8i32;
    let scissor_w = 16i32;
    let scissor_h = 16i32;

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red using the fast-path load-op clear.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });

        // Scissored clear to green.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];

    // Inside scissor should be green.
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x + 1) as u32, (scissor_y + 1) as u32),
        green
    );
    assert_eq!(
        pixel_at(
            &rgba,
            width,
            (scissor_x + scissor_w - 1) as u32,
            (scissor_y + scissor_h - 1) as u32
        ),
        green
    );

    // Outside scissor should remain red.
    assert_eq!(pixel_at(&rgba, width, 0, 0), red);
    assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), red);
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x - 1) as u32, scissor_y as u32),
        red
    );
    assert_eq!(
        pixel_at(
            &rgba,
            width,
            (scissor_x + scissor_w) as u32,
            scissor_y as u32
        ),
        red
    );
}
