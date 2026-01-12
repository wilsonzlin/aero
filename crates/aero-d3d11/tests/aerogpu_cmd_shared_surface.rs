mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
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

#[test]
fn aerogpu_cmd_shared_surface_release_retires_token_but_keeps_alias_valid() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x0123_4567_89AB_CDEF;

        // Submission 1:
        // - Create texture
        // - Export + Import alias
        // - Bind alias as render target
        // - Clear to green
        // - Release token (retire it; future imports/exports must fail)
        // - Present (should still work via alias)
        let mut stream = Vec::new();
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

        // RELEASE_SHARED_SURFACE (TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ReleaseSharedSurface as u32);
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
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
            "presented render target should still be the underlying texture after RELEASE_SHARED_SURFACE"
        );

        let pixels = exec.read_texture_rgba8(presented).await.unwrap();
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }

        // Submission 2: token should no longer be importable.
        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // out_resource_handle
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("IMPORT_SHARED_SURFACE") && msg.contains("unknown share_token"),
            "expected IMPORT_SHARED_SURFACE to fail after token release, got: {msg}"
        );

        // Submission 3: token should not be reusable for export.
        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes()); // resource_handle
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("EXPORT_SHARED_SURFACE") && msg.contains("previously released"),
            "expected EXPORT_SHARED_SURFACE to fail after token release, got: {msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_shared_surface_release_unknown_token_is_noop() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x1111_2222_3333_4444;

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (TEX)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RELEASE_SHARED_SURFACE before export: should be a no-op (must not retire).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ReleaseSharedSurface as u32);
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes());
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE should still succeed.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE should succeed.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (color0=ALIAS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        for _ in 1..8 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // CLEAR (green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

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
        assert_eq!(presented, TEX);

        let pixels = exec.read_texture_rgba8(presented).await.unwrap();
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_shared_surface_reusing_underlying_handle_while_alias_alive_is_an_error() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x0BAD_F00D_DEAD_BEEF;

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (TEX)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE (TEX -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE (ALIAS -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // DESTROY_RESOURCE (TEX): original handle is destroyed, but alias keeps the underlying alive.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        // Buggy guest behavior: attempt to reuse the underlying handle while the alias is still alive.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("CREATE_TEXTURE2D") && msg.contains("still in use"),
            "expected CREATE_TEXTURE2D handle reuse to be rejected, got: {msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_shared_surface_import_into_destroyed_original_handle_is_an_error() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x0123_4567_89AB_CDEF;

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (TEX)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE (TEX -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE (ALIAS -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // DESTROY_RESOURCE (TEX): original handle is destroyed, but alias keeps the underlying alive.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        // Buggy guest behavior: attempt to re-import into the destroyed original handle id.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes()); // out_resource_handle (destroyed original)
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("IMPORT_SHARED_SURFACE") && msg.contains("still in use"),
            "expected IMPORT_SHARED_SURFACE handle reuse to be rejected, got: {msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_shared_surface_using_destroyed_original_handle_is_an_error() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x0123_4567_89AB_CDEF;

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (TEX)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE (TEX -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE (ALIAS -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // DESTROY_RESOURCE (TEX): original handle is destroyed, but alias keeps the underlying alive.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        // Buggy guest behavior: attempt to use the destroyed original handle in subsequent commands.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&TEX.to_le_bytes()); // colors[0] (destroyed original)
        for _ in 1..8 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SET_RENDER_TARGETS") && msg.contains("destroyed"),
            "expected use-after-destroy to be rejected, got: {msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_shared_surface_destroy_resource_refcounts_aliases() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x0BAD_F00D_DEAD_BEEF;

        let mut guest_mem = VecGuestMemory::new(0);

        // Submission 1: create + import alias, clear green, present.
        let mut stream = Vec::new();
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
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE (TEX -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE (ALIAS -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (color0=ALIAS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&ALIAS.to_le_bytes()); // colors[0]
        for _ in 1..8 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // CLEAR (green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        // Ensure alias reads back the same underlying green texture.
        let pixels = exec.read_texture_rgba8(ALIAS).await.unwrap();
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }

        // Submission 2: drop the original handle, then clear red via alias and present.
        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // DESTROY_RESOURCE (TEX) - alias still alive.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (color0=ALIAS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        for _ in 1..8 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // CLEAR (red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("destroy original + clear via alias should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(ALIAS).await.unwrap();
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }

        // Submission 3: destroy alias (final ref) - underlying should be destroyed.
        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("destroy alias should succeed");
        exec.poll_wait();

        assert!(
            exec.texture_size(TEX).is_err(),
            "expected underlying texture to be destroyed after final handle release"
        );
    });
}

#[test]
fn aerogpu_cmd_shared_surface_double_destroy_of_original_handle_is_idempotent() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x0123_4567_89AB_CDEF;

        // Single submission:
        // - create + export + import alias
        // - destroy original handle twice (should be idempotent as long as alias is alive)
        // - clear via alias and present
        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (TEX)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE (TEX -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE (ALIAS -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // DESTROY_RESOURCE (TEX) - original handle release while alias still alive.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        // Duplicate destroy: should be a no-op (must not destroy the underlying resource while
        // aliases are alive).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (color0=ALIAS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        for _ in 1..8 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // CLEAR (green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        end_cmd(&mut stream, start);

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
        assert_eq!(presented, TEX);

        // The host reports the underlying handle as the presented render target. Even if the
        // original handle was destroyed, the underlying ID remains valid for host-side use as long
        // as aliases keep it alive.
        let pixels = exec.read_texture_rgba8(presented).await.unwrap();
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_shared_surface_rejects_creating_resource_under_alias_handle() {
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
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;
        const TOKEN: u64 = 0x0123_4567_89AB_CDEF;

        let mut stream = Vec::new();
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
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // EXPORT_SHARED_SURFACE (TEX -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ExportSharedSurface as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // IMPORT_SHARED_SURFACE (ALIAS -> TOKEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ImportSharedSurface as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&TOKEN.to_le_bytes());
        end_cmd(&mut stream, start);

        // Buggy guest behavior: attempt to create a new texture using the alias handle.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&ALIAS.to_le_bytes()); // texture_handle (alias)
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&WIDTH.to_le_bytes());
        stream.extend_from_slice(&HEIGHT.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes());
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("CREATE_TEXTURE2D") && msg.contains("alias"),
            "expected CREATE_TEXTURE2D under alias handle to be rejected, got: {msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_shared_surface_rejects_creating_buffer_over_existing_texture_handle() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const HANDLE: u32 = 1;
        const WIDTH: u32 = 4;
        const HEIGHT: u32 = 4;

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (HANDLE)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&HANDLE.to_le_bytes()); // texture_handle
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

        // Buggy guest behavior: attempt to create a buffer that reuses the same handle as the
        // existing texture.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&HANDLE.to_le_bytes()); // buffer_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("CREATE_BUFFER") && msg.contains("still in use"),
            "expected CREATE_BUFFER under an existing resource handle to be rejected, got: {msg}"
        );
    });
}
