#![cfg(target_arch = "wasm32")]

use crate::common;
use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_gpu::AerogpuD3d9Executor;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

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

#[wasm_bindgen_test(async)]
async fn aerogpu_d3d9_writeback_copy_buffer_and_texture() {
    let mut exec = match AerogpuD3d9Executor::new_headless().await {
        Ok(exec) => exec,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err})"));
            return;
        }
    };

    let alloc_table = AllocTable::new([(
        1u32,
        AllocEntry {
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x8000,
        },
    )])
    .expect("create alloc table");

    // -----------------------------------------------------------------------------
    // Buffer writeback
    // -----------------------------------------------------------------------------
    {
        const SRC: u32 = 1;
        const DST: u32 = 2;

        let src_backing_offset = 0u32;
        let dst_backing_offset = 0x200u32;
        let buf_size = 256u64;

        let copy_src_offset = 16u64;
        let copy_dst_offset = 32u64;
        let copy_size = 64u64;

        let mut guest_mem = VecGuestMemory::new(0x10000);
        let src_pattern: Vec<u8> = (0u8..=255u8).collect();
        guest_mem
            .write(0x100 + src_backing_offset as u64, &src_pattern)
            .expect("write src backing");
        guest_mem
            .write(0x100 + dst_backing_offset as u64, &[0xEEu8; 256])
            .expect("write dst backing");

        let mut stream = Vec::new();
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
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&src_backing_offset.to_le_bytes());
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_BUFFER DST
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&buf_size.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
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

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream_with_guest_memory_for_context_async(
            0,
            &stream,
            &mut guest_mem,
            Some(&alloc_table),
        )
        .await
        .expect("execute_cmd_stream_async should succeed");

        let dst_base = (0x100 + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        let actual = &mem[dst_base + copy_dst_offset as usize
            ..dst_base + (copy_dst_offset + copy_size) as usize];
        let expected =
            &src_pattern[copy_src_offset as usize..(copy_src_offset + copy_size) as usize];
        assert_eq!(actual, expected);
        assert_eq!(mem[dst_base], 0xEE);
        assert_eq!(mem[dst_base + (copy_dst_offset + copy_size) as usize], 0xEE);
    }

    exec.reset();

    // -----------------------------------------------------------------------------
    // Texture writeback (ensure only the copied rect is committed)
    // -----------------------------------------------------------------------------
    {
        const SRC: u32 = 1;
        const DST: u32 = 2;

        let width = 4u32;
        let height = 4u32;
        let row_pitch = 32u32;
        let texture_bytes_len = (row_pitch as usize) * (height as usize);

        let dst_backing_offset = 0x400u32;

        let dst_x = 1u32;
        let dst_y = 1u32;
        let copy_w = 2u32;
        let copy_h = 2u32;

        let mut guest_mem = VecGuestMemory::new(0x10000);
        guest_mem
            .write(
                0x100 + dst_backing_offset as u64,
                &vec![0x11u8; texture_bytes_len],
            )
            .expect("write dst texture bytes");

        let mut stream = Vec::new();
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
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
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
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&copy_w.to_le_bytes());
        stream.extend_from_slice(&copy_h.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream_with_guest_memory_for_context_async(
            0,
            &stream,
            &mut guest_mem,
            Some(&alloc_table),
        )
        .await
        .expect("execute_cmd_stream_async should succeed");

        let green = [0u8, 255u8, 0u8, 255u8];
        let mut expected = vec![0x11u8; texture_bytes_len];
        for row in 0..copy_h {
            for col in 0..copy_w {
                let dx = dst_x + col;
                let dy = dst_y + row;
                let dst_idx = dy as usize * row_pitch as usize + dx as usize * 4;
                expected[dst_idx..dst_idx + 4].copy_from_slice(&green);
            }
        }

        let dst_base = (0x100 + dst_backing_offset as u64) as usize;
        let mem = guest_mem.as_slice();
        assert_eq!(
            &mem[dst_base..dst_base + expected.len()],
            expected.as_slice()
        );
    }
}
