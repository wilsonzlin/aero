use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_COPY_FLAG_WRITEBACK_DST,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

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
fn copy_buffer_writeback_roundtrip() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping buffer writeback test");
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

        let guest_mem = VecGuestMemory::new(0x2000);

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

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
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
fn copy_texture2d_writeback_roundtrip() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping texture writeback test");
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

        let guest_mem = VecGuestMemory::new(0x8000);
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

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
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
