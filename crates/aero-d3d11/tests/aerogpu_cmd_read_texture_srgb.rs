mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::{
    AerogpuFormat, AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32,
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

const RT: u32 = 1;
const WIDTH: u32 = 2;
const HEIGHT: u32 = 2;

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

fn build_clear_present_stream_with_color(format_u32: u32, rgba: [f32; 4]) -> Vec<u8> {
    let mut stream = Vec::new();
    // Stream header (24 bytes)
    stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

    // CREATE_TEXTURE2D (RT)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&RT.to_le_bytes()); // texture_handle
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
    stream.extend_from_slice(&format_u32.to_le_bytes());
    stream.extend_from_slice(&WIDTH.to_le_bytes());
    stream.extend_from_slice(&HEIGHT.to_le_bytes());
    stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
    stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
    stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_RENDER_TARGETS (RT)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
    stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
    stream.extend_from_slice(&RT.to_le_bytes()); // colors[0]
    for _ in 0..7 {
        stream.extend_from_slice(&0u32.to_le_bytes()); // colors[1..]
    }
    end_cmd(&mut stream, start);

    // CLEAR (solid red)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
    stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
    stream.extend_from_slice(&rgba[0].to_bits().to_le_bytes()); // r
    stream.extend_from_slice(&rgba[1].to_bits().to_le_bytes()); // g
    stream.extend_from_slice(&rgba[2].to_bits().to_le_bytes()); // b
    stream.extend_from_slice(&rgba[3].to_bits().to_le_bytes()); // a
    stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
    stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
    end_cmd(&mut stream, start);

    // PRESENT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    end_cmd(&mut stream, start);

    // Patch stream size in header.
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());

    stream
}

fn build_clear_present_stream(format_u32: u32) -> Vec<u8> {
    build_clear_present_stream_with_color(format_u32, [1.0, 0.0, 0.0, 1.0])
}

#[test]
fn aerogpu_cmd_read_texture_rgba8_supports_srgb_render_targets() {
    // ABI 1.2 adds sRGB format variants; skip on older ABI versions.
    if AEROGPU_ABI_MINOR < 2 {
        eprintln!(
            "skipping {}: requires AeroGPU ABI 1.2+ (sRGB format variants)",
            module_path!()
        );
        return;
    }

    let bgra_srgb = AerogpuFormat::B8G8R8A8UnormSrgb as u32;
    let rgba_srgb = AerogpuFormat::R8G8B8A8UnormSrgb as u32;

    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        for (label, format_u32) in [("bgra", bgra_srgb), ("rgba", rgba_srgb)] {
            exec.reset();

            let stream = build_clear_present_stream(format_u32);
            let mut guest_mem = VecGuestMemory::new(0);
            let report = exec
                .execute_cmd_stream(&stream, None, &mut guest_mem)
                .unwrap_or_else(|e| panic!("execute_cmd_stream failed for {label}: {e:#}"));
            exec.poll_wait();

            let presented = report
                .presents
                .last()
                .and_then(|p| p.presented_render_target)
                .expect("stream should present a render target");
            assert_eq!(presented, RT, "unexpected presented render target");

            let pixels = exec
                .read_texture_rgba8(RT)
                .await
                .unwrap_or_else(|e| panic!("read_texture_rgba8 failed for {label}: {e:#}"));
            assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);
            for px in pixels.chunks_exact(4) {
                assert_eq!(px, &[255, 0, 0, 255]);
            }
        }
    });
}

#[test]
fn aerogpu_cmd_srgb_render_target_encodes_linear_clear_values() {
    // ABI 1.2 adds sRGB format variants; skip on older ABI versions.
    if AEROGPU_ABI_MINOR < 2 {
        eprintln!(
            "skipping {}: requires AeroGPU ABI 1.2+ (sRGB format variants)",
            module_path!()
        );
        return;
    }

    // Clear takes linear float values, but storing to an sRGB render target should encode to sRGB.
    //
    // For linear 0.5, the sRGB encoded value is ~0.735, which quantizes to ~188.
    let expected_r = 188u8;

    let bgra_srgb = AerogpuFormat::B8G8R8A8UnormSrgb as u32;
    let rgba_srgb = AerogpuFormat::R8G8B8A8UnormSrgb as u32;

    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        for (label, format_u32) in [("bgra", bgra_srgb), ("rgba", rgba_srgb)] {
            exec.reset();

            let stream = build_clear_present_stream_with_color(format_u32, [0.5, 0.0, 0.0, 1.0]);
            let mut guest_mem = VecGuestMemory::new(0);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .unwrap_or_else(|e| panic!("execute_cmd_stream failed for {label}: {e:#}"));
            exec.poll_wait();

            let pixels = exec
                .read_texture_rgba8(RT)
                .await
                .unwrap_or_else(|e| panic!("read_texture_rgba8 failed for {label}: {e:#}"));
            assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);

            for px in pixels.chunks_exact(4) {
                assert!(
                    px[0].abs_diff(expected_r) <= 2,
                    "r mismatch: got={} expected~={} ({label})",
                    px[0],
                    expected_r
                );
                assert!(px[1] <= 2, "g mismatch: got={} ({label})", px[1]);
                assert!(px[2] <= 2, "b mismatch: got={} ({label})", px[2]);
                assert!(px[3].abs_diff(255) <= 2, "a mismatch: got={} ({label})", px[3]);
            }
        }
    });
}
