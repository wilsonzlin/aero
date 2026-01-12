mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

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

#[test]
fn aerogpu_cmd_shared_surface_import_aliases_underlying_texture() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const TEX: u32 = 1;
        const ALIAS: u32 = 2;
        const WIDTH: u32 = 8;
        const HEIGHT: u32 = 8;
        const TOKEN: u64 = 0x0123_4567_89AB_CDEF;

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (TEX)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes()); // format
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes (not allocation-backed)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE (TEX -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes()); // resource_handle
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE (ALIAS -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes()); // out_resource_handle
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (color0=ALIAS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&ALIAS.to_le_bytes()); // colors[0]
        for _ in 1..8 {
            stream.extend_from_slice(&0u32.to_le_bytes()); // colors[1..]
        }
        end_cmd(&mut stream, start);

        // CLEAR (green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes()); // flags
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // r
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // g
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // b
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // a
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
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

        let mut guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let presented = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("expected a presented render target");
        assert_eq!(
            presented, TEX,
            "presented render target should resolve alias handle to underlying texture"
        );

        let (w, h) = exec.texture_size(presented).unwrap();
        assert_eq!((w, h), (WIDTH, HEIGHT));

        let pixels = exec.read_texture_rgba8(presented).await.unwrap();
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}

