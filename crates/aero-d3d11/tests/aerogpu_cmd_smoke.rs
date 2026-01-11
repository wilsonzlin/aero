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

fn end_cmd(stream: &mut Vec<u8>, start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
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

        let guest_mem = VecGuestMemory::new(0);
        let report = exec
            .execute_cmd_stream(CMD_TRIANGLE_SM4, None, &guest_mem)
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

        let guest_mem = VecGuestMemory::new(0x1000);
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

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
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
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x01, 0x02, 0x03, 0x04, 0xAA,
            0xBB, 0xCC, 0xDD,
        ];

        let guest_mem = VecGuestMemory::new(0x1000);
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

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
            .unwrap();
        exec.poll_wait();

        let mut out = [0u8; 16];
        guest_mem.read(dst_gpa, &mut out).unwrap();
        assert_eq!(out, src_pixels);
    });
}
