#![cfg(target_arch = "wasm32")]

use crate::common;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::{GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_COPY_FLAG_WRITEBACK_DST, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use wasm_bindgen_test::*;

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
async fn aerogpu_cmd_writeback_dst_updates_guest_memory_on_wasm() {
    let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
        Ok(exec) => exec,
        Err(e) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
            return;
        }
    };

    const BUF_SRC: u32 = 1;
    const BUF_DST: u32 = 2;
    const TEX_SRC: u32 = 3;
    const TEX_DST: u32 = 4;

    let buf_bytes: [u8; 16] = *b"hello aero-gpu!!";
    let buf_size_bytes = buf_bytes.len() as u64;

    const TEX_WIDTH: u32 = 3;
    const TEX_HEIGHT: u32 = 2;
    const TEX_ROW_PITCH: u32 = 16;
    const TEX_SIZE_BYTES: usize = (TEX_ROW_PITCH as usize) * (TEX_HEIGHT as usize);
    const TEX_UNPADDED_BPR: usize = (TEX_WIDTH as usize) * 4;

    let mut src_tex = vec![0u8; TEX_SIZE_BYTES];
    let mut dst_tex = vec![0x55u8; TEX_SIZE_BYTES];
    for row in 0..TEX_HEIGHT as usize {
        let base = row * TEX_ROW_PITCH as usize;
        for i in 0..TEX_UNPADDED_BPR {
            src_tex[base + i] = (row as u8).wrapping_mul(0x10).wrapping_add(i as u8);
        }
        // Fill the padding bytes with known values; they should not be overwritten by writeback.
        for i in TEX_UNPADDED_BPR..TEX_ROW_PITCH as usize {
            src_tex[base + i] = 0xEE;
            dst_tex[base + i] = 0xDD;
        }
    }

    let mut guest_mem = VecGuestMemory::new(0x10000);

    let buf_src_alloc_id = 1u32;
    let buf_dst_alloc_id = 2u32;
    let tex_src_alloc_id = 3u32;
    let tex_dst_alloc_id = 4u32;

    let buf_src_gpa = 0x100u64;
    let buf_dst_gpa = 0x200u64;
    let tex_src_gpa = 0x300u64;
    let tex_dst_gpa = 0x400u64;

    guest_mem.write(buf_src_gpa, &buf_bytes).unwrap();
    guest_mem.write(buf_dst_gpa, &[0u8; 16]).unwrap();
    guest_mem.write(tex_src_gpa, &src_tex).unwrap();
    guest_mem.write(tex_dst_gpa, &dst_tex).unwrap();

    let allocs = [
        AerogpuAllocEntry {
            alloc_id: buf_src_alloc_id,
            flags: 0,
            gpa: buf_src_gpa,
            size_bytes: buf_size_bytes,
            reserved0: 0,
        },
        AerogpuAllocEntry {
            alloc_id: buf_dst_alloc_id,
            flags: 0,
            gpa: buf_dst_gpa,
            size_bytes: buf_size_bytes,
            reserved0: 0,
        },
        AerogpuAllocEntry {
            alloc_id: tex_src_alloc_id,
            flags: 0,
            gpa: tex_src_gpa,
            size_bytes: TEX_SIZE_BYTES as u64,
            reserved0: 0,
        },
        AerogpuAllocEntry {
            alloc_id: tex_dst_alloc_id,
            flags: 0,
            gpa: tex_dst_gpa,
            size_bytes: TEX_SIZE_BYTES as u64,
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
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&BUF_SRC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&buf_size_bytes.to_le_bytes());
    stream.extend_from_slice(&buf_src_alloc_id.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // RESOURCE_DIRTY_RANGE (full SRC buffer)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
    stream.extend_from_slice(&BUF_SRC.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&buf_size_bytes.to_le_bytes()); // size_bytes
    end_cmd(&mut stream, start);

    // CREATE_BUFFER (DST, guest-backed)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&BUF_DST.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&buf_size_bytes.to_le_bytes());
    stream.extend_from_slice(&buf_dst_alloc_id.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // COPY_BUFFER (SRC -> DST) with WRITEBACK_DST.
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
    stream.extend_from_slice(&BUF_DST.to_le_bytes());
    stream.extend_from_slice(&BUF_SRC.to_le_bytes());
    stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
    stream.extend_from_slice(&buf_size_bytes.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // CREATE_TEXTURE2D (SRC, guest-backed)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&TEX_SRC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
    stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
    stream.extend_from_slice(&TEX_WIDTH.to_le_bytes());
    stream.extend_from_slice(&TEX_HEIGHT.to_le_bytes());
    stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
    stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
    stream.extend_from_slice(&TEX_ROW_PITCH.to_le_bytes());
    stream.extend_from_slice(&tex_src_alloc_id.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // RESOURCE_DIRTY_RANGE (mark SRC texture dirty)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
    stream.extend_from_slice(&TEX_SRC.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&(TEX_SIZE_BYTES as u64).to_le_bytes()); // size_bytes
    end_cmd(&mut stream, start);

    // CREATE_TEXTURE2D (DST, guest-backed)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&TEX_DST.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
    stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
    stream.extend_from_slice(&TEX_WIDTH.to_le_bytes());
    stream.extend_from_slice(&TEX_HEIGHT.to_le_bytes());
    stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
    stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
    stream.extend_from_slice(&TEX_ROW_PITCH.to_le_bytes());
    stream.extend_from_slice(&tex_dst_alloc_id.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // COPY_TEXTURE2D (SRC -> DST) with WRITEBACK_DST.
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
    stream.extend_from_slice(&TEX_DST.to_le_bytes());
    stream.extend_from_slice(&TEX_SRC.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
    stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
    stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
    stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
    stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
    stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
    stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
    stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
    stream.extend_from_slice(&TEX_WIDTH.to_le_bytes());
    stream.extend_from_slice(&TEX_HEIGHT.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // Patch stream size in header.
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());

    exec.execute_cmd_stream_async(&stream, Some(&allocs), &mut guest_mem)
        .await
        .unwrap();

    let mut out_buf = [0u8; 16];
    guest_mem.read(buf_dst_gpa, &mut out_buf).unwrap();
    assert_eq!(out_buf, buf_bytes);

    let mut out_tex = vec![0u8; TEX_SIZE_BYTES];
    guest_mem.read(tex_dst_gpa, &mut out_tex).unwrap();
    let mut expected = dst_tex;
    for row in 0..TEX_HEIGHT as usize {
        let base = row * TEX_ROW_PITCH as usize;
        expected[base..base + TEX_UNPADDED_BPR]
            .copy_from_slice(&src_tex[base..base + TEX_UNPADDED_BPR]);
    }
    assert_eq!(out_tex, expected);
}
