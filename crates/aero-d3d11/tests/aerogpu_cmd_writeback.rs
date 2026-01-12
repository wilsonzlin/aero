mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AEROGPU_ALLOC_FLAG_READONLY};

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
fn copy_buffer_writeback_roundtrip() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x200u32;
        let buf_size = 256u64;

        let copy_src_offset = 16u64;
        let copy_dst_offset = 32u64;
        let copy_size = 64u64;

        let mut guest_mem = VecGuestMemory::new(0x2000);

        let src_pattern: Vec<u8> = (0u8..=255u8).collect();
        assert_eq!(src_pattern.len(), buf_size as usize);
        guest_mem
            .write(alloc.gpa + src_backing_offset as u64, &src_pattern)
            .expect("write src backing");

        let dst_init = vec![0xEEu8; buf_size as usize];
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_init)
            .expect("write dst backing");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER DST
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE src
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // COPY_BUFFER (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&copy_dst_offset.to_le_bytes());
        stream.extend_from_slice(&copy_src_offset.to_le_bytes());
        stream.extend_from_slice(&copy_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        let actual = &mem[dst_base + copy_dst_offset as usize
            ..dst_base + (copy_dst_offset + copy_size) as usize];
        let expected =
            &src_pattern[copy_src_offset as usize..(copy_src_offset + copy_size) as usize];
        assert_eq!(actual, expected);

        // Ensure bytes outside the copied range were not clobbered.
        assert_eq!(mem[dst_base], 0xEE);
        assert_eq!(mem[dst_base + (copy_dst_offset + copy_size) as usize], 0xEE);
    });
}

#[test]
fn copy_buffer_writeback_roundtrip_async() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x200u32;
        let buf_size = 256u64;

        let copy_src_offset = 16u64;
        let copy_dst_offset = 32u64;
        let copy_size = 64u64;

        let mut guest_mem = VecGuestMemory::new(0x2000);

        let src_pattern: Vec<u8> = (0u8..=255u8).collect();
        assert_eq!(src_pattern.len(), buf_size as usize);
        guest_mem
            .write(alloc.gpa + src_backing_offset as u64, &src_pattern)
            .expect("write src backing");

        let dst_init = vec![0xEEu8; buf_size as usize];
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_init)
            .expect("write dst backing");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER DST
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE src
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // COPY_BUFFER (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&copy_dst_offset.to_le_bytes());
        stream.extend_from_slice(&copy_src_offset.to_le_bytes());
        stream.extend_from_slice(&copy_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream_async(&stream, Some(&allocs), &mut guest_mem)
            .await
            .expect("execute_cmd_stream_async should succeed");
        exec.poll_wait();

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        let actual = &mem[dst_base + copy_dst_offset as usize
            ..dst_base + (copy_dst_offset + copy_size) as usize];
        let expected =
            &src_pattern[copy_src_offset as usize..(copy_src_offset + copy_size) as usize];
        assert_eq!(actual, expected);

        // Ensure bytes outside the copied range were not clobbered.
        assert_eq!(mem[dst_base], 0xEE);
        assert_eq!(mem[dst_base + (copy_dst_offset + copy_size) as usize], 0xEE);
    });
}

#[test]
fn copy_buffer_writeback_allows_unaligned_size_at_buffer_end() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        // Not COPY_BUFFER_ALIGNMENT-aligned.
        let buf_size = 6u64;
        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x100u32;

        let mut guest_mem = VecGuestMemory::new(0x2000);
        let src_bytes = [0u8, 1, 2, 3, 4, 5];
        let dst_bytes = [0xEEu8; 6];
        guest_mem
            .write(alloc.gpa + src_backing_offset as u64, &src_bytes)
            .expect("write src backing");
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_bytes)
            .expect("write dst backing");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER DST
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER (WRITEBACK_DST) with an unaligned size.
        //
        // This is allowed as long as the copy reaches the end of both buffers (wgpu requires a
        // 4-byte-aligned copy size, so the executor pads to the underlying aligned allocation).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        assert_eq!(&mem[dst_base..dst_base + buf_size as usize], &src_bytes);
    });
}

#[test]
fn copy_texture2d_writeback_roundtrip() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x4000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let width = 7u32;
        let height = 4u32;
        let row_pitch = 32u32; // larger than width*4; not wgpu-aligned

        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x200u32;

        let dst_x = 1u32;
        let dst_y = 2u32;
        let src_x = 2u32;
        let src_y = 1u32;
        let copy_w = 3u32;
        let copy_h = 2u32;

        let texture_bytes_len = (row_pitch as usize) * (height as usize);

        let mut src_bytes = vec![0x77u8; texture_bytes_len];
        for y in 0..height {
            for x in 0..width {
                let idx = y as usize * row_pitch as usize + x as usize * 4;
                src_bytes[idx] = x as u8;
                src_bytes[idx + 1] = y as u8;
                src_bytes[idx + 2] = x.wrapping_add(y) as u8;
                src_bytes[idx + 3] = 0xFF;
            }
        }

        let dst_bytes = vec![0x11u8; texture_bytes_len];

        let mut guest_mem = VecGuestMemory::new(0x8000);
        guest_mem
            .write(alloc.gpa + src_backing_offset as u64, &src_bytes)
            .expect("write src texture bytes");
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_bytes)
            .expect("write dst texture bytes");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D DST
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE src
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(texture_bytes_len as u64).to_le_bytes());
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&DST.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&dst_x.to_le_bytes());
        stream.extend_from_slice(&dst_y.to_le_bytes());
        stream.extend_from_slice(&src_x.to_le_bytes());
        stream.extend_from_slice(&src_y.to_le_bytes());
        stream.extend_from_slice(&copy_w.to_le_bytes());
        stream.extend_from_slice(&copy_h.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let mut expected_dst = dst_bytes;
        for row in 0..copy_h {
            for col in 0..copy_w {
                let sx = src_x + col;
                let sy = src_y + row;
                let dx = dst_x + col;
                let dy = dst_y + row;

                let src_idx = sy as usize * row_pitch as usize + sx as usize * 4;
                let dst_idx = dy as usize * row_pitch as usize + dx as usize * 4;
                expected_dst[dst_idx..dst_idx + 4]
                    .copy_from_slice(&src_bytes[src_idx..src_idx + 4]);
            }
        }

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        let actual = &mem[dst_base..dst_base + expected_dst.len()];
        assert_eq!(actual, expected_dst.as_slice());
    });
}

#[test]
fn copy_texture2d_writeback_encodes_x8_alpha_as_255() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x4000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let width = 4u32;
        let height = 4u32;
        let row_pitch = 32u32; // larger than width*4; not wgpu-aligned

        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x200u32;

        let dst_x = 0u32;
        let dst_y = 0u32;
        let src_x = 0u32;
        let src_y = 0u32;
        let copy_w = width;
        let copy_h = height;

        let texture_bytes_len = (row_pitch as usize) * (height as usize);
        let formats = [
            AerogpuFormat::R8G8B8X8Unorm,
            AerogpuFormat::R8G8B8X8UnormSrgb,
            AerogpuFormat::B8G8R8X8Unorm,
            AerogpuFormat::B8G8R8X8UnormSrgb,
        ];

        for format in formats {
            exec.reset();

            let mut src_bytes = vec![0x77u8; texture_bytes_len];
            for y in 0..height {
                for x in 0..width {
                    let idx = y as usize * row_pitch as usize + x as usize * 4;
                    src_bytes[idx] = x as u8;
                    src_bytes[idx + 1] = y as u8;
                    src_bytes[idx + 2] = x.wrapping_add(y) as u8;
                    // X8 format: set a non-opaque alpha byte to ensure the executor forces it to
                    // 0xFF on writeback.
                    src_bytes[idx + 3] = 0x00;
                }
            }

            let dst_bytes = vec![0x11u8; texture_bytes_len];

            let mut guest_mem = VecGuestMemory::new(0x8000);
            guest_mem
                .write(alloc.gpa + src_backing_offset as u64, &src_bytes)
                .expect("write src texture bytes");
            guest_mem
                .write(alloc.gpa + dst_backing_offset as u64, &dst_bytes)
                .expect("write dst texture bytes");

            let mut stream = Vec::new();
            // Stream header (24 bytes)
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            // CREATE_TEXTURE2D SRC
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&SRC.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&(format as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&row_pitch.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&src_backing_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // CREATE_TEXTURE2D DST
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&DST.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&(format as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&row_pitch.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // RESOURCE_DIRTY_RANGE src
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
            stream.extend_from_slice(&SRC.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&(texture_bytes_len as u64).to_le_bytes());
            end_cmd(&mut stream, start);

            // COPY_TEXTURE2D (WRITEBACK_DST)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
            stream.extend_from_slice(&DST.to_le_bytes()); // dst_texture
            stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
            stream.extend_from_slice(&dst_x.to_le_bytes());
            stream.extend_from_slice(&dst_y.to_le_bytes());
            stream.extend_from_slice(&src_x.to_le_bytes());
            stream.extend_from_slice(&src_y.to_le_bytes());
            stream.extend_from_slice(&copy_w.to_le_bytes());
            stream.extend_from_slice(&copy_h.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // Patch stream size in header.
            let total_size = stream.len() as u32;
            stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&total_size.to_le_bytes());

            exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
                .unwrap_or_else(|e| {
                    panic!("execute_cmd_stream should succeed for {format:?}: {e:#}")
                });
            exec.poll_wait();

            let mut expected_dst = dst_bytes;
            for row in 0..copy_h {
                for col in 0..copy_w {
                    let sx = src_x + col;
                    let sy = src_y + row;
                    let dx = dst_x + col;
                    let dy = dst_y + row;

                    let src_idx = sy as usize * row_pitch as usize + sx as usize * 4;
                    let dst_idx = dy as usize * row_pitch as usize + dx as usize * 4;
                    expected_dst[dst_idx..dst_idx + 3]
                        .copy_from_slice(&src_bytes[src_idx..src_idx + 3]);
                    expected_dst[dst_idx + 3] = 0xFF;
                }
            }

            let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
            let mem = guest_mem.as_slice();
            let actual = &mem[dst_base..dst_base + expected_dst.len()];
            assert_eq!(actual, expected_dst.as_slice(), "format {format:?}");
        }
    });
}

#[test]
fn x8_texture_upload_forces_alpha_to_255() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x4000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let width = 4u32;
        let height = 4u32;
        let row_pitch = 32u32;

        let src_backing_offset = 0u32;
        let texture_bytes_len = (row_pitch as usize) * (height as usize);
        let formats = [
            AerogpuFormat::R8G8B8X8Unorm,
            AerogpuFormat::R8G8B8X8UnormSrgb,
            AerogpuFormat::B8G8R8X8Unorm,
            AerogpuFormat::B8G8R8X8UnormSrgb,
        ];

        for format in formats {
            exec.reset();

            // X8 format: guest writes arbitrary alpha bytes, but GPU sampling should observe alpha=1.
            let mut src_bytes = vec![0x77u8; texture_bytes_len];
            for y in 0..height {
                for x in 0..width {
                    let idx = y as usize * row_pitch as usize + x as usize * 4;
                    src_bytes[idx] = x as u8;
                    src_bytes[idx + 1] = y as u8;
                    src_bytes[idx + 2] = x.wrapping_add(y) as u8;
                    src_bytes[idx + 3] = 0x00;
                }
            }

            let mut guest_mem = VecGuestMemory::new(0x8000);
            guest_mem
                .write(alloc.gpa + src_backing_offset as u64, &src_bytes)
                .expect("write src texture bytes");

            let mut stream = Vec::new();
            // Stream header (24 bytes)
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            // CREATE_TEXTURE2D SRC (guest-backed).
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&SRC.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&(format as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&row_pitch.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&src_backing_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // CREATE_TEXTURE2D DST (host-owned).
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&DST.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&(format as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // COPY_TEXTURE2D (no writeback). This will force the SRC upload before copying.
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
            stream.extend_from_slice(&DST.to_le_bytes()); // dst_texture
            stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // Patch stream size in header.
            let total_size = stream.len() as u32;
            stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&total_size.to_le_bytes());

            exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
                .unwrap_or_else(|e| {
                    panic!("execute_cmd_stream should succeed for {format:?}: {e:#}")
                });
            exec.poll_wait();

            let actual = exec
                .read_texture_rgba8(DST)
                .await
                .expect("read_texture_rgba8");

            let mut expected = vec![0u8; (width * height * 4) as usize];
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    match format {
                        AerogpuFormat::B8G8R8X8Unorm | AerogpuFormat::B8G8R8X8UnormSrgb => {
                            // Guest bytes are BGRA; readback is normalized to RGBA.
                            expected[idx] = x.wrapping_add(y) as u8;
                            expected[idx + 1] = y as u8;
                            expected[idx + 2] = x as u8;
                        }
                        _ => {
                            expected[idx] = x as u8;
                            expected[idx + 1] = y as u8;
                            expected[idx + 2] = x.wrapping_add(y) as u8;
                        }
                    }
                    expected[idx + 3] = 0xFF;
                }
            }

            assert_eq!(
                actual, expected,
                "format {format:?}: X8 upload should force alpha to opaque"
            );
        }
    });
}

#[test]
fn copy_texture2d_writeback_roundtrip_async() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x4000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let width = 7u32;
        let height = 4u32;
        let row_pitch = 32u32; // larger than width*4; not wgpu-aligned

        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x200u32;

        let dst_x = 1u32;
        let dst_y = 2u32;
        let src_x = 2u32;
        let src_y = 1u32;
        let copy_w = 3u32;
        let copy_h = 2u32;

        let texture_bytes_len = (row_pitch as usize) * (height as usize);

        let mut src_bytes = vec![0x77u8; texture_bytes_len];
        for y in 0..height {
            for x in 0..width {
                let idx = y as usize * row_pitch as usize + x as usize * 4;
                src_bytes[idx] = x as u8;
                src_bytes[idx + 1] = y as u8;
                src_bytes[idx + 2] = x.wrapping_add(y) as u8;
                src_bytes[idx + 3] = 0xFF;
            }
        }

        let dst_bytes = vec![0x11u8; texture_bytes_len];

        let mut guest_mem = VecGuestMemory::new(0x8000);
        guest_mem
            .write(alloc.gpa + src_backing_offset as u64, &src_bytes)
            .expect("write src texture bytes");
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_bytes)
            .expect("write dst texture bytes");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D DST
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE src
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(texture_bytes_len as u64).to_le_bytes());
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D (WRITEBACK_DST)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&DST.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&dst_x.to_le_bytes());
        stream.extend_from_slice(&dst_y.to_le_bytes());
        stream.extend_from_slice(&src_x.to_le_bytes());
        stream.extend_from_slice(&src_y.to_le_bytes());
        stream.extend_from_slice(&copy_w.to_le_bytes());
        stream.extend_from_slice(&copy_h.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream_async(&stream, Some(&allocs), &mut guest_mem)
            .await
            .expect("execute_cmd_stream_async should succeed");
        exec.poll_wait();

        let mut expected_dst = dst_bytes;
        for row in 0..copy_h {
            for col in 0..copy_w {
                let sx = src_x + col;
                let sy = src_y + row;
                let dx = dst_x + col;
                let dy = dst_y + row;

                let src_idx = sy as usize * row_pitch as usize + sx as usize * 4;
                let dst_idx = dy as usize * row_pitch as usize + dx as usize * 4;
                expected_dst[dst_idx..dst_idx + 4]
                    .copy_from_slice(&src_bytes[src_idx..src_idx + 4]);
            }
        }

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        let actual = &mem[dst_base..dst_base + expected_dst.len()];
        assert_eq!(actual, expected_dst.as_slice());
    });
}

#[test]
fn copy_texture2d_writeback_does_not_clobber_uncopied_pixels() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x4000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let width = 7u32;
        let height = 4u32;
        let row_pitch = 32u32; // larger than width*4; not wgpu-aligned

        let dst_backing_offset = 0x200u32;

        let dst_x = 1u32;
        let dst_y = 1u32;
        let copy_w = 3u32;
        let copy_h = 2u32;

        let texture_bytes_len = (row_pitch as usize) * (height as usize);
        let dst_bytes = vec![0x11u8; texture_bytes_len];

        let mut guest_mem = VecGuestMemory::new(0x8000);
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_bytes)
            .expect("write dst texture bytes");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D SRC (host allocated, renderable)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS -> SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&SRC.to_le_bytes()); // colors[0]
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // CLEAR SRC to green (GPU-only)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // r
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // g
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // b
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // a
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D DST (guest-backed, renderable)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS -> DST
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&DST.to_le_bytes()); // colors[0]
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // CLEAR DST to red (GPU-only; guest memory stays 0x11)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // r
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // g
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // b
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // a
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D (WRITEBACK_DST) from green SRC onto red DST.
        // WRITEBACK_DST should commit only the copied rect back into guest memory; pixels outside
        // the rect must not be clobbered by the pre-copy clear.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&DST.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&dst_x.to_le_bytes());
        stream.extend_from_slice(&dst_y.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&copy_w.to_le_bytes());
        stream.extend_from_slice(&copy_h.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let green = [0u8, 255u8, 0u8, 255u8];
        let mut expected_dst = dst_bytes;
        for row in 0..copy_h {
            for col in 0..copy_w {
                let dx = dst_x + col;
                let dy = dst_y + row;
                let dst_idx = dy as usize * row_pitch as usize + dx as usize * 4;
                expected_dst[dst_idx..dst_idx + 4].copy_from_slice(&green);
            }
        }

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        let actual = &mem[dst_base..dst_base + expected_dst.len()];
        assert_eq!(actual, expected_dst.as_slice());
    });
}

#[test]
fn copy_buffer_clears_dst_dirty_after_copy() {
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
        const OUT: u32 = 3;

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let buf_size = 64u64;
        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x100u32;
        let out_backing_offset = 0x200u32;

        let mut guest_mem = VecGuestMemory::new(0x2000);

        let src_pattern = vec![0xAAu8; buf_size as usize];
        let dst_pattern = vec![0x11u8; buf_size as usize];
        guest_mem
            .write(alloc.gpa + src_backing_offset as u64, &src_pattern)
            .expect("write src backing");
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_pattern)
            .expect("write dst backing");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER DST (guest-backed; starts dirty)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER OUT (guest-backed for writeback)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&OUT.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&out_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER: SRC -> DST (no writeback; should clear DST dirty).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_BUFFER: DST -> OUT (WRITEBACK_DST). If DST.dirty is not cleared, the executor will
        // re-upload stale guest memory (dst_pattern) and the writeback will observe the wrong data.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&OUT.to_le_bytes());
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let out_base = (alloc.gpa + out_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        assert_eq!(
            &mem[out_base..out_base + buf_size as usize],
            src_pattern.as_slice()
        );
    });
}

#[test]
fn copy_texture2d_clears_dst_dirty_after_copy() {
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
        const OUT: u32 = 3;

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let width = 2u32;
        let height = 2u32;
        let row_pitch = width * 4;
        let texture_bytes_len = (row_pitch as usize) * (height as usize);

        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x100u32;
        let out_backing_offset = 0x200u32;

        let red_px = [0xFFu8, 0x00u8, 0x00u8, 0xFFu8];
        let green_px = [0x00u8, 0xFFu8, 0x00u8, 0xFFu8];
        let src_bytes = red_px.repeat((width * height) as usize);
        let dst_bytes = green_px.repeat((width * height) as usize);
        assert_eq!(src_bytes.len(), texture_bytes_len);
        assert_eq!(dst_bytes.len(), texture_bytes_len);

        let mut guest_mem = VecGuestMemory::new(0x2000);
        guest_mem
            .write(alloc.gpa + src_backing_offset as u64, &src_bytes)
            .expect("write src texture bytes");
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_bytes)
            .expect("write dst texture bytes");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D SRC
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D DST (guest-backed; starts dirty)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D OUT (guest-backed for writeback)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&OUT.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&out_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D: SRC -> DST (no writeback; should clear DST dirty).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
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
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D: DST -> OUT (WRITEBACK_DST). If DST.dirty is not cleared, the executor will
        // re-upload stale guest memory (dst_bytes) and the writeback will observe the wrong data.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&OUT.to_le_bytes());
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let out_base = (alloc.gpa + out_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        assert_eq!(
            &mem[out_base..out_base + texture_bytes_len],
            src_bytes.as_slice()
        );
    });
}

#[test]
fn copy_buffer_writeback_requires_alloc_table_each_submit() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let buf_size = 64u64;
        let dst_backing_offset = 0x200u32;

        let mut guest_mem = VecGuestMemory::new(0x2000);
        let dst_init = vec![0xEEu8; buf_size as usize];
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_init)
            .expect("write dst init");

        let src_pattern: Vec<u8> = (0u8..(buf_size as u8)).collect();

        // First submission: create SRC/DST and upload SRC data.
        {
            let mut stream = Vec::new();
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            // CREATE_BUFFER SRC (host-owned)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&SRC.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&buf_size.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // CREATE_BUFFER DST (guest-backed)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&DST.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&buf_size.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // UPLOAD_RESOURCE SRC data.
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
            stream.extend_from_slice(&SRC.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&buf_size.to_le_bytes());
            stream.extend_from_slice(&src_pattern);
            end_cmd(&mut stream, start);

            // Patch stream size in header.
            let total_size = stream.len() as u32;
            stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&total_size.to_le_bytes());

            exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
                .expect("execute_cmd_stream should succeed");
            exec.poll_wait();
        }

        // Second submission: COPY_BUFFER with WRITEBACK_DST but no alloc table.
        let mut copy_stream = Vec::new();
        copy_stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        copy_stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        copy_stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        copy_stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        copy_stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        copy_stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut copy_stream, AerogpuCmdOpcode::CopyBuffer as u32);
        copy_stream.extend_from_slice(&DST.to_le_bytes());
        copy_stream.extend_from_slice(&SRC.to_le_bytes());
        copy_stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        copy_stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        copy_stream.extend_from_slice(&buf_size.to_le_bytes());
        copy_stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        copy_stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut copy_stream, start);

        let total_size = copy_stream.len() as u32;
        copy_stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&copy_stream, None, &mut guest_mem)
            .expect_err("expected missing alloc table error");
        exec.poll_wait();

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        assert_eq!(
            &mem[dst_base..dst_base + buf_size as usize],
            dst_init.as_slice()
        );

        // Third submission: alloc table present but missing the required alloc_id.
        let missing = AerogpuAllocEntry {
            alloc_id: 2,
            flags: 0,
            gpa: 0x1000,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let missing_allocs = [missing];
        exec.execute_cmd_stream(&copy_stream, Some(&missing_allocs), &mut guest_mem)
            .expect_err("expected missing alloc_id error");
        exec.poll_wait();
        let mem = guest_mem.as_slice();
        assert_eq!(
            &mem[dst_base..dst_base + buf_size as usize],
            dst_init.as_slice()
        );

        // Fourth submission: provide the correct alloc table; writeback should succeed.
        exec.execute_cmd_stream(&copy_stream, Some(&allocs), &mut guest_mem)
            .expect("writeback should succeed with alloc table");
        exec.poll_wait();
        let mem = guest_mem.as_slice();
        assert_eq!(
            &mem[dst_base..dst_base + buf_size as usize],
            src_pattern.as_slice()
        );
    });
}

#[test]
fn copy_buffer_writeback_rejects_readonly_alloc() {
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

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: AEROGPU_ALLOC_FLAG_READONLY,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let buf_size = 64u64;
        let dst_backing_offset = 0x200u32;

        let mut guest_mem = VecGuestMemory::new(0x2000);
        let dst_init = vec![0xEEu8; buf_size as usize];
        guest_mem
            .write(alloc.gpa + dst_backing_offset as u64, &dst_init)
            .expect("write dst init");

        let src_pattern: Vec<u8> = (0u8..(buf_size as u8)).collect();

        // Create SRC/DST and upload SRC data.
        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER SRC (host-owned)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER DST (guest-backed; alloc is READONLY)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
        stream.extend_from_slice(&dst_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE SRC data.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&src_pattern);
        end_cmd(&mut stream, start);

        // COPY_BUFFER (WRITEBACK_DST) should be rejected due to READONLY alloc.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect_err("expected READONLY writeback error");
        exec.poll_wait();

        let dst_base = (alloc.gpa + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        assert_eq!(
            &mem[dst_base..dst_base + buf_size as usize],
            dst_init.as_slice()
        );
    });
}
