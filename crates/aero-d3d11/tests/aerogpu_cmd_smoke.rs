mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::{GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_COPY_FLAG_WRITEBACK_DST, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

const CMD_TRIANGLE_SM4: &[u8] = include_bytes!("fixtures/cmd_triangle_sm4.bin");

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
const OPCODE_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;
const OPCODE_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

const OPCODE_SET_RASTERIZER_STATE: u32 = AerogpuCmdOpcode::SetRasterizerState as u32;
const OPCODE_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

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

fn rgba_within(got: &[u8], expected: [u8; 4], tol: u8) -> bool {
    got.len() == 4 && got.iter().zip(expected).all(|(&g, e)| g.abs_diff(e) <= tol)
}

fn patch_first_viewport_rect(bytes: &mut [u8], x: f32, y: f32, width: f32, height: f32) {
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut patched = false;
    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size == 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::SetViewport as u32 {
            // `struct aerogpu_cmd_set_viewport` stores float bits as u32s:
            // hdr(8) + x/y/width/height/min/max (6 * 4).
            assert_eq!(size, 32, "unexpected SetViewport size");
            let x_off = cursor + 8;
            let y_off = cursor + 12;
            let width_off = cursor + 16;
            let height_off = cursor + 20;
            bytes[x_off..x_off + 4].copy_from_slice(&x.to_bits().to_le_bytes());
            bytes[y_off..y_off + 4].copy_from_slice(&y.to_bits().to_le_bytes());
            bytes[width_off..width_off + 4].copy_from_slice(&width.to_bits().to_le_bytes());
            bytes[height_off..height_off + 4].copy_from_slice(&height.to_bits().to_le_bytes());
            patched = true;
            break;
        }

        cursor += size;
    }

    assert!(patched, "failed to find SetViewport command to patch");
}

fn patch_first_viewport(bytes: &mut [u8], width: f32, height: f32) {
    patch_first_viewport_rect(bytes, 0.0, 0.0, width, height);
}

fn insert_viewport_reset_and_duplicate_last_draw(bytes: &mut Vec<u8>) {
    // Find the last draw packet and the first present packet so we can insert:
    //   SET_VIEWPORT (0x0)  // reset to default
    //   <duplicate draw>
    // just before present.
    //
    // This is used to validate that a degenerate viewport properly restores the
    // default viewport *within the same render pass* (viewport state persists in
    // wgpu until changed).
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut last_draw: Option<(usize, usize)> = None;
    let mut present_offset: Option<usize> = None;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size < ProtocolCmdHdr::SIZE_BYTES || size % 4 != 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Draw as u32 || opcode == AerogpuCmdOpcode::DrawIndexed as u32
        {
            last_draw = Some((cursor, size));
        }
        if opcode == AerogpuCmdOpcode::Present as u32
            || opcode == AerogpuCmdOpcode::PresentEx as u32
        {
            present_offset = Some(cursor);
            break;
        }

        cursor += size;
    }

    let (draw_off, draw_size) = last_draw.expect("expected fixture to contain a draw packet");
    let present_off = present_offset.expect("expected fixture to contain a present packet");
    let draw_bytes = bytes[draw_off..draw_off + draw_size].to_vec();

    let mut insert = Vec::with_capacity(32 + draw_bytes.len());

    // SET_VIEWPORT (degenerate reset: width=0, height=0).
    insert.extend_from_slice(&(AerogpuCmdOpcode::SetViewport as u32).to_le_bytes());
    insert.extend_from_slice(&32u32.to_le_bytes());
    insert.extend_from_slice(&0u32.to_le_bytes()); // x bits
    insert.extend_from_slice(&0u32.to_le_bytes()); // y bits
    insert.extend_from_slice(&0u32.to_le_bytes()); // width bits
    insert.extend_from_slice(&0u32.to_le_bytes()); // height bits
    insert.extend_from_slice(&0u32.to_le_bytes()); // min_depth bits
    insert.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth bits

    // Duplicate the last draw so the second draw should use the reset viewport.
    insert.extend_from_slice(&draw_bytes);

    bytes.splice(present_off..present_off, insert);

    // Patch stream size in header.
    let total_size = bytes.len() as u32;
    bytes[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
}

fn insert_viewport_nan_and_duplicate_last_draw(bytes: &mut Vec<u8>) {
    // Similar to `insert_viewport_reset_and_duplicate_last_draw`, but uses a NaN viewport size to
    // ensure the executor treats invalid viewport payloads as a reset to the default viewport
    // instead of leaving stale state within the render pass.
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut last_draw: Option<(usize, usize)> = None;
    let mut present_offset: Option<usize> = None;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size < ProtocolCmdHdr::SIZE_BYTES || size % 4 != 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Draw as u32 || opcode == AerogpuCmdOpcode::DrawIndexed as u32
        {
            last_draw = Some((cursor, size));
        }
        if opcode == AerogpuCmdOpcode::Present as u32
            || opcode == AerogpuCmdOpcode::PresentEx as u32
        {
            present_offset = Some(cursor);
            break;
        }

        cursor += size;
    }

    let (draw_off, draw_size) = last_draw.expect("expected fixture to contain a draw packet");
    let present_off = present_offset.expect("expected fixture to contain a present packet");
    let draw_bytes = bytes[draw_off..draw_off + draw_size].to_vec();

    let mut insert = Vec::with_capacity(32 + draw_bytes.len());

    // SET_VIEWPORT with NaN width/height.
    let nan = f32::NAN.to_bits();
    insert.extend_from_slice(&(AerogpuCmdOpcode::SetViewport as u32).to_le_bytes());
    insert.extend_from_slice(&32u32.to_le_bytes());
    insert.extend_from_slice(&0u32.to_le_bytes()); // x bits
    insert.extend_from_slice(&0u32.to_le_bytes()); // y bits
    insert.extend_from_slice(&nan.to_le_bytes()); // width bits
    insert.extend_from_slice(&nan.to_le_bytes()); // height bits
    insert.extend_from_slice(&0u32.to_le_bytes()); // min_depth bits
    insert.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth bits

    insert.extend_from_slice(&draw_bytes);

    bytes.splice(present_off..present_off, insert);

    // Patch stream size in header.
    let total_size = bytes.len() as u32;
    bytes[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
}

fn insert_viewport_oob_and_duplicate_last_draw(bytes: &mut Vec<u8>, x: f32, y: f32, width: f32, height: f32) {
    // Insert an out-of-bounds viewport update + duplicate the last draw before Present. This is
    // used to validate that a *valid* viewport which becomes empty after clamping to the render
    // target causes the draw to be skipped (D3D semantics) instead of being treated as a protocol
    // reset-to-default.
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut last_draw: Option<(usize, usize)> = None;
    let mut present_offset: Option<usize> = None;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size < ProtocolCmdHdr::SIZE_BYTES || size % 4 != 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Draw as u32 || opcode == AerogpuCmdOpcode::DrawIndexed as u32 {
            last_draw = Some((cursor, size));
        }
        if opcode == AerogpuCmdOpcode::Present as u32 || opcode == AerogpuCmdOpcode::PresentEx as u32 {
            present_offset = Some(cursor);
            break;
        }

        cursor += size;
    }

    let (draw_off, draw_size) = last_draw.expect("expected fixture to contain a draw packet");
    let present_off = present_offset.expect("expected fixture to contain a present packet");
    let draw_bytes = bytes[draw_off..draw_off + draw_size].to_vec();

    let mut insert = Vec::with_capacity(32 + draw_bytes.len());

    insert.extend_from_slice(&(AerogpuCmdOpcode::SetViewport as u32).to_le_bytes());
    insert.extend_from_slice(&32u32.to_le_bytes());
    insert.extend_from_slice(&x.to_bits().to_le_bytes());
    insert.extend_from_slice(&y.to_bits().to_le_bytes());
    insert.extend_from_slice(&width.to_bits().to_le_bytes());
    insert.extend_from_slice(&height.to_bits().to_le_bytes());
    insert.extend_from_slice(&0u32.to_le_bytes()); // min_depth bits
    insert.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth bits

    insert.extend_from_slice(&draw_bytes);

    bytes.splice(present_off..present_off, insert);

    // Patch stream size in header.
    let total_size = bytes.len() as u32;
    bytes[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
}

fn insert_scissor_enable_and_rect_before_first_draw(bytes: &mut Vec<u8>, width: i32, height: i32) {
    insert_scissor_enable_and_rect_before_first_draw_xy(bytes, 0, 0, width, height);
}

fn insert_scissor_enable_and_rect_before_first_draw_xy(
    bytes: &mut Vec<u8>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    // Insert:
    //   SET_RASTERIZER_STATE (scissor_enable=1)
    //   SET_SCISSOR (0,0,width,height)
    // immediately before the first draw so the render pass starts with a clipped scissor.
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut first_draw: Option<usize> = None;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size < ProtocolCmdHdr::SIZE_BYTES || size % 4 != 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Draw as u32 || opcode == AerogpuCmdOpcode::DrawIndexed as u32
        {
            first_draw = Some(cursor);
            break;
        }

        cursor += size;
    }

    let draw_off = first_draw.expect("expected fixture to contain a draw packet");

    let mut insert = Vec::with_capacity(32 + 24);

    // SET_RASTERIZER_STATE (32 bytes).
    //
    // We only care about `scissor_enable`; the remaining fields are set to safe defaults.
    insert.extend_from_slice(&OPCODE_SET_RASTERIZER_STATE.to_le_bytes());
    insert.extend_from_slice(&32u32.to_le_bytes());
    insert.extend_from_slice(&0u32.to_le_bytes()); // fill_mode (solid)
    insert.extend_from_slice(&0u32.to_le_bytes()); // cull_mode (none)
    insert.extend_from_slice(&0u32.to_le_bytes()); // front_ccw (false)
    insert.extend_from_slice(&1u32.to_le_bytes()); // scissor_enable (true)
    insert.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
    insert.extend_from_slice(&0u32.to_le_bytes()); // flags

    // SET_SCISSOR (24 bytes).
    insert.extend_from_slice(&OPCODE_SET_SCISSOR.to_le_bytes());
    insert.extend_from_slice(&24u32.to_le_bytes());
    insert.extend_from_slice(&x.to_le_bytes());
    insert.extend_from_slice(&y.to_le_bytes());
    insert.extend_from_slice(&width.to_le_bytes());
    insert.extend_from_slice(&height.to_le_bytes());

    bytes.splice(draw_off..draw_off, insert);
}

fn insert_scissor_reset_and_duplicate_last_draw(bytes: &mut Vec<u8>) {
    // Find the last draw packet and the first present packet so we can insert:
    //   SET_SCISSOR (0x0)  // reset/disable
    //   <duplicate draw>
    // just before present.
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut last_draw: Option<(usize, usize)> = None;
    let mut present_offset: Option<usize> = None;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size < ProtocolCmdHdr::SIZE_BYTES || size % 4 != 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Draw as u32 || opcode == AerogpuCmdOpcode::DrawIndexed as u32
        {
            last_draw = Some((cursor, size));
        }
        if opcode == AerogpuCmdOpcode::Present as u32
            || opcode == AerogpuCmdOpcode::PresentEx as u32
        {
            present_offset = Some(cursor);
            break;
        }

        cursor += size;
    }

    let (draw_off, draw_size) = last_draw.expect("expected fixture to contain a draw packet");
    let present_off = present_offset.expect("expected fixture to contain a present packet");
    let draw_bytes = bytes[draw_off..draw_off + draw_size].to_vec();

    let mut insert = Vec::with_capacity(24 + draw_bytes.len());

    // SET_SCISSOR (degenerate reset: width=0, height=0).
    insert.extend_from_slice(&OPCODE_SET_SCISSOR.to_le_bytes());
    insert.extend_from_slice(&24u32.to_le_bytes());
    insert.extend_from_slice(&0i32.to_le_bytes()); // x
    insert.extend_from_slice(&0i32.to_le_bytes()); // y
    insert.extend_from_slice(&0i32.to_le_bytes()); // width
    insert.extend_from_slice(&0i32.to_le_bytes()); // height

    // Duplicate the last draw so the second draw should use the reset scissor.
    insert.extend_from_slice(&draw_bytes);

    bytes.splice(present_off..present_off, insert);
}

fn insert_scissor_disable_and_duplicate_last_draw(bytes: &mut Vec<u8>) {
    // Find the last draw packet and the first present packet so we can insert:
    //   SET_RASTERIZER_STATE (scissor_enable=0)  // disable scissor test
    //   <duplicate draw>
    // just before present.
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut last_draw: Option<(usize, usize)> = None;
    let mut present_offset: Option<usize> = None;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size < ProtocolCmdHdr::SIZE_BYTES || size % 4 != 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Draw as u32 || opcode == AerogpuCmdOpcode::DrawIndexed as u32
        {
            last_draw = Some((cursor, size));
        }
        if opcode == AerogpuCmdOpcode::Present as u32
            || opcode == AerogpuCmdOpcode::PresentEx as u32
        {
            present_offset = Some(cursor);
            break;
        }

        cursor += size;
    }

    let (draw_off, draw_size) = last_draw.expect("expected fixture to contain a draw packet");
    let present_off = present_offset.expect("expected fixture to contain a present packet");
    let draw_bytes = bytes[draw_off..draw_off + draw_size].to_vec();

    let mut insert = Vec::with_capacity(32 + draw_bytes.len());

    // SET_RASTERIZER_STATE (32 bytes) with scissor_enable=0.
    insert.extend_from_slice(&OPCODE_SET_RASTERIZER_STATE.to_le_bytes());
    insert.extend_from_slice(&32u32.to_le_bytes());
    insert.extend_from_slice(&0u32.to_le_bytes()); // fill_mode (solid)
    insert.extend_from_slice(&0u32.to_le_bytes()); // cull_mode (none)
    insert.extend_from_slice(&0u32.to_le_bytes()); // front_ccw (false)
    insert.extend_from_slice(&0u32.to_le_bytes()); // scissor_enable (false)
    insert.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
    insert.extend_from_slice(&0u32.to_le_bytes()); // flags

    // Duplicate the last draw so the second draw should run with scissor disabled.
    insert.extend_from_slice(&draw_bytes);

    bytes.splice(present_off..present_off, insert);
}

fn insert_scissor_oob_and_duplicate_last_draw(bytes: &mut Vec<u8>, x: i32, y: i32, width: i32, height: i32) {
    // Insert a scissor rect that becomes empty after clamping to the render target, plus a
    // duplicate draw, immediately before Present. This validates that an empty scissor causes the
    // draw to be skipped instead of being treated as "scissor disabled".
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut last_draw: Option<(usize, usize)> = None;
    let mut present_offset: Option<usize> = None;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size < ProtocolCmdHdr::SIZE_BYTES || size % 4 != 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Draw as u32 || opcode == AerogpuCmdOpcode::DrawIndexed as u32 {
            last_draw = Some((cursor, size));
        }
        if opcode == AerogpuCmdOpcode::Present as u32 || opcode == AerogpuCmdOpcode::PresentEx as u32 {
            present_offset = Some(cursor);
            break;
        }

        cursor += size;
    }

    let (draw_off, draw_size) = last_draw.expect("expected fixture to contain a draw packet");
    let present_off = present_offset.expect("expected fixture to contain a present packet");
    let draw_bytes = bytes[draw_off..draw_off + draw_size].to_vec();

    let mut insert = Vec::with_capacity(24 + draw_bytes.len());

    insert.extend_from_slice(&OPCODE_SET_SCISSOR.to_le_bytes());
    insert.extend_from_slice(&24u32.to_le_bytes());
    insert.extend_from_slice(&x.to_le_bytes());
    insert.extend_from_slice(&y.to_le_bytes());
    insert.extend_from_slice(&width.to_le_bytes());
    insert.extend_from_slice(&height.to_le_bytes());

    insert.extend_from_slice(&draw_bytes);

    bytes.splice(present_off..present_off, insert);
}

#[test]
fn aerogpu_cmd_renders_solid_red_triangle_fixture() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(CMD_TRIANGLE_SM4, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");

        let (width, height) = exec
            .texture_size(render_target)
            .expect("presented render target should exist");
        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), width as usize * height as usize * 4);

        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_renders_triangle_fixture_with_small_viewport() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        // Patch the viewport from 64x64 to 32x32 so the draw covers only part of the RT, leaving
        // the rest at the clear color.
        patch_first_viewport(&mut stream, 32.0, 32.0);

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // Inside viewport -> red triangle.
        assert_eq!(px(16, 16), &[255, 0, 0, 255]);

        // Outside viewport -> clear color (0.1, 0.2, 0.3, 1.0) in UNORM8 (tolerate rounding).
        assert!(
            rgba_within(px(48, 48), [26, 51, 77, 255], 1),
            "unexpected clear pixel {:?}",
            px(48, 48)
        );
    });
}

#[test]
fn aerogpu_cmd_viewport_out_of_bounds_draws_nothing() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        // Move the viewport completely out of the render target. D3D11 should draw nothing (wgpu
        // cannot represent an empty viewport, so the executor must skip draws).
        patch_first_viewport_rect(&mut stream, -1000.0, 0.0, 100.0, 64.0);

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // All pixels should remain the clear color (0.1, 0.2, 0.3, 1.0) in UNORM8.
        assert!(
            rgba_within(px(16, 16), [26, 51, 77, 255], 1),
            "unexpected pixel {:?}",
            px(16, 16)
        );
        assert!(
            rgba_within(px(48, 48), [26, 51, 77, 255], 1),
            "unexpected pixel {:?}",
            px(48, 48)
        );
    });
}

#[test]
fn aerogpu_cmd_viewport_out_of_bounds_within_pass_draws_nothing() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        patch_first_viewport(&mut stream, 32.0, 32.0);
        insert_viewport_oob_and_duplicate_last_draw(&mut stream, -1000.0, 0.0, 100.0, 64.0);

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // Initial (32x32) viewport draw should still render inside the viewport.
        assert_eq!(px(16, 16), &[255, 0, 0, 255]);

        // Second draw uses an out-of-bounds viewport; it should draw nothing, leaving this pixel
        // (outside the first viewport) at the clear color.
        assert!(
            rgba_within(px(48, 48), [26, 51, 77, 255], 1),
            "unexpected pixel {:?}",
            px(48, 48)
        );
    });
}

#[test]
fn aerogpu_cmd_viewport_reset_restores_default_within_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        // Shrink the first viewport so the first draw affects only a sub-rect.
        patch_first_viewport(&mut stream, 32.0, 32.0);
        // Insert a degenerate viewport reset and a second draw (before Present). The second draw
        // should restore a full-target viewport and paint over pixels outside the first viewport.
        insert_viewport_reset_and_duplicate_last_draw(&mut stream);

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // This pixel is outside the initial 32x32 viewport, so it should be the clear color after
        // the first draw. After the viewport reset + second draw, it must be red.
        assert_eq!(px(48, 48), &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_viewport_nan_restores_default_within_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        patch_first_viewport(&mut stream, 32.0, 32.0);
        insert_viewport_nan_and_duplicate_last_draw(&mut stream);

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // Outside the initial 32x32 viewport: after the NaN viewport update + second draw, the
        // executor should have restored the default viewport and this pixel must be red.
        assert_eq!(px(48, 48), &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_renders_triangle_fixture_with_small_scissor() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        insert_scissor_enable_and_rect_before_first_draw(&mut stream, 32, 32);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // Inside scissor -> red triangle.
        assert_eq!(px(16, 16), &[255, 0, 0, 255]);

        // Outside scissor -> clear color (0.1, 0.2, 0.3, 1.0) in UNORM8 (tolerate rounding).
        assert!(
            rgba_within(px(48, 48), [26, 51, 77, 255], 1),
            "unexpected clear pixel {:?}",
            px(48, 48)
        );
    });
}

#[test]
fn aerogpu_cmd_scissor_out_of_bounds_draws_nothing() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        // Enable scissor test and set a rect completely out of the render target.
        insert_scissor_enable_and_rect_before_first_draw_xy(&mut stream, 1000, 0, 10, 10);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        assert!(
            rgba_within(px(16, 16), [26, 51, 77, 255], 1),
            "unexpected pixel {:?}",
            px(16, 16)
        );
        assert!(
            rgba_within(px(48, 48), [26, 51, 77, 255], 1),
            "unexpected pixel {:?}",
            px(48, 48)
        );
    });
}

#[test]
fn aerogpu_cmd_scissor_out_of_bounds_within_pass_draws_nothing() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        insert_scissor_enable_and_rect_before_first_draw(&mut stream, 32, 32);
        insert_scissor_oob_and_duplicate_last_draw(&mut stream, 1000, 0, 10, 10);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // Initial (32x32) scissor should still render inside the scissor.
        assert_eq!(px(16, 16), &[255, 0, 0, 255]);

        // Second draw uses an out-of-bounds scissor; it should draw nothing, leaving this pixel
        // (outside the first scissor) at the clear color.
        assert!(
            rgba_within(px(48, 48), [26, 51, 77, 255], 1),
            "unexpected pixel {:?}",
            px(48, 48)
        );
    });
}

#[test]
fn aerogpu_cmd_scissor_reset_restores_default_within_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        insert_scissor_reset_and_duplicate_last_draw(&mut stream);
        insert_scissor_enable_and_rect_before_first_draw(&mut stream, 32, 32);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // This pixel is outside the initial 32x32 scissor rect, so it should remain the clear
        // color after the first draw. After the scissor reset + second draw, it must be red.
        assert_eq!(px(48, 48), &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_scissor_disable_restores_default_within_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        insert_scissor_enable_and_rect_before_first_draw(&mut stream, 32, 32);
        insert_scissor_disable_and_duplicate_last_draw(&mut stream);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");
        let (width, height) = exec.texture_size(render_target).unwrap();
        assert_eq!((width, height), (64, 64));

        let pixels = exec.read_texture_rgba8(render_target).await.unwrap();
        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // Outside the initial 32x32 scissor rect: the first draw shouldn't touch this pixel, but
        // after scissor is disabled + a second draw occurs, it must be red.
        assert_eq!(px(48, 48), &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_copy_buffer_writeback_updates_guest_memory() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const SRC: u32 = 1;
        const DST: u32 = 2;
        let bytes: [u8; 16] = *b"hello aero-gpu!!";
        let size_bytes = bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let src_alloc_id = 1u32;
        let dst_alloc_id = 2u32;
        let src_gpa = 0x100u64;
        let dst_gpa = 0x200u64;
        guest_mem.write(src_gpa, &bytes).unwrap();
        guest_mem.write(dst_gpa, &[0u8; 16]).unwrap();

        let allocs = [
            AerogpuAllocEntry {
                alloc_id: src_alloc_id,
                flags: 0,
                gpa: src_gpa,
                size_bytes,
                reserved0: 0,
            },
            AerogpuAllocEntry {
                alloc_id: dst_alloc_id,
                flags: 0,
                gpa: dst_gpa,
                size_bytes,
                reserved0: 0,
            },
        ];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (SRC, guest-backed)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER).to_le_bytes());
        stream.extend_from_slice(&size_bytes.to_le_bytes());
        stream.extend_from_slice(&src_alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full SRC buffer)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&size_bytes.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (DST, guest-backed)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER).to_le_bytes());
        stream.extend_from_slice(&size_bytes.to_le_bytes());
        stream.extend_from_slice(&dst_alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER (SRC -> DST) with WRITEBACK_DST.
        let start = begin_cmd(&mut stream, OPCODE_COPY_BUFFER);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&size_bytes.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let mut out = [0u8; 16];
        guest_mem.read(dst_gpa, &mut out).unwrap();
        assert_eq!(out, bytes);
    });
}

#[test]
fn aerogpu_cmd_copy_texture2d_writeback_updates_guest_memory() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const SRC: u32 = 1;
        const DST: u32 = 2;
        const WIDTH: u32 = 2;
        const HEIGHT: u32 = 2;
        const ROW_PITCH: u32 = WIDTH * 4;
        const TEX_SIZE: u64 = (ROW_PITCH as u64) * (HEIGHT as u64);

        let src_pixels: [u8; 16] = [
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x01, 0x02, 0x03, 0x04, 0xAA, 0xBB,
            0xCC, 0xDD,
        ];

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let src_alloc_id = 1u32;
        let dst_alloc_id = 2u32;
        let src_gpa = 0x100u64;
        let dst_gpa = 0x200u64;
        guest_mem.write(src_gpa, &src_pixels).unwrap();
        guest_mem.write(dst_gpa, &[0u8; 16]).unwrap();

        let allocs = [
            AerogpuAllocEntry {
                alloc_id: src_alloc_id,
                flags: 0,
                gpa: src_gpa,
                size_bytes: TEX_SIZE,
                reserved0: 0,
            },
            AerogpuAllocEntry {
                alloc_id: dst_alloc_id,
                flags: 0,
                gpa: dst_gpa,
                size_bytes: TEX_SIZE,
                reserved0: 0,
            },
        ];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (SRC, guest-backed)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&ROW_PITCH.to_le_bytes());
        stream.extend_from_slice(&src_alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (mark SRC dirty)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&TEX_SIZE.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (DST, guest-backed)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&ROW_PITCH.to_le_bytes());
        stream.extend_from_slice(&dst_alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D (SRC -> DST) with WRITEBACK_DST.
        let start = begin_cmd(&mut stream, OPCODE_COPY_TEXTURE2D);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let mut out = [0u8; 16];
        guest_mem.read(dst_gpa, &mut out).unwrap();
        assert_eq!(out, src_pixels);
    });
}

#[test]
fn aerogpu_cmd_copy_buffer_validates_bounds() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const BUF_A: u32 = 1;
        const BUF_B: u32 = 2;

        let mut guest_mem = VecGuestMemory::new(0);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (BUF_A, host alloc)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&BUF_A.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER).to_le_bytes());
        stream.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (BUF_B, host alloc)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&BUF_B.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER).to_le_bytes());
        stream.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER (out of bounds)
        let start = begin_cmd(&mut stream, OPCODE_COPY_BUFFER);
        stream.extend_from_slice(&BUF_B.to_le_bytes());
        stream.extend_from_slice(&BUF_A.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&32u64.to_le_bytes()); // size_bytes (too large)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("COPY_BUFFER"), "unexpected error: {msg}");
    });
}

#[test]
fn aerogpu_cmd_copy_texture2d_validates_bounds() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const SRC: u32 = 1;
        const DST: u32 = 2;

        let mut guest_mem = VecGuestMemory::new(0);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for handle in [SRC, DST] {
            let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes());
            stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
            stream.extend_from_slice(&2u32.to_le_bytes()); // width
            stream.extend_from_slice(&2u32.to_le_bytes()); // height
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // COPY_TEXTURE2D (out of bounds: width=3 overflows 2x2 texture)
        let start = begin_cmd(&mut stream, OPCODE_COPY_TEXTURE2D);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&3u32.to_le_bytes()); // width (too large)
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("COPY_TEXTURE2D"),
            "unexpected error (missing COPY_TEXTURE2D): {msg}"
        );
        assert!(
            msg.contains("out of bounds"),
            "unexpected error (missing bounds context): {msg}"
        );

        // Also validate that the executor rejects invalid subresource indices.
        exec.reset();

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for handle in [SRC, DST] {
            let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes());
            stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
            stream.extend_from_slice(&2u32.to_le_bytes()); // width
            stream.extend_from_slice(&2u32.to_le_bytes()); // height
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // COPY_TEXTURE2D (invalid mip level: src_mip_level=1 but mip_levels=1).
        let start = begin_cmd(&mut stream, OPCODE_COPY_TEXTURE2D);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&1u32.to_le_bytes()); // src_mip_level (out of range)
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("COPY_TEXTURE2D"),
            "unexpected error (missing COPY_TEXTURE2D): {msg}"
        );
        assert!(
            msg.contains("src_mip_level") && msg.contains("out of range"),
            "unexpected error (missing mip validation): {msg}"
        );

        // Array layer validation.
        exec.reset();

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for handle in [SRC, DST] {
            let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes());
            stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
            stream.extend_from_slice(&2u32.to_le_bytes()); // width
            stream.extend_from_slice(&2u32.to_le_bytes()); // height
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // COPY_TEXTURE2D (invalid array layer: src_array_layer=1 but array_layers=1).
        let start = begin_cmd(&mut stream, OPCODE_COPY_TEXTURE2D);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&1u32.to_le_bytes()); // src_array_layer (out of range)
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("COPY_TEXTURE2D"),
            "unexpected error (missing COPY_TEXTURE2D): {msg}"
        );
        assert!(
            msg.contains("src_array_layer") && msg.contains("out of range"),
            "unexpected error (missing array layer validation): {msg}"
        );
    });
}
