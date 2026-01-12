mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_RESOURCE_USAGE_TEXTURE,
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

fn end_cmd(stream: &mut [u8], start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

#[test]
fn create_texture2d_requires_row_pitch_for_backed_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let allocs = [AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes (invalid)
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x2000);
        let err = exec
            .execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect_err("expected CREATE_TEXTURE2D to reject missing row_pitch_bytes");
        assert!(
            err.to_string()
                .contains("row_pitch_bytes is required for allocation-backed textures"),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn create_texture2d_validates_all_mips_against_allocation_size() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Backing allocation is only large enough for mip0 (64 bytes), but the texture declares
        // mip_levels=2 so validation should fail.
        let allocs = [AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 16 * 4, // row_pitch_bytes * height (mip0 only)
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&2u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&16u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x2000);
        let err = exec
            .execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect_err("expected CREATE_TEXTURE2D to reject undersized allocation");
        assert!(
            err.to_string().contains("out of range"),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn create_texture2d_guest_backed_mips_are_tightly_packed_for_levels_gt_0() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // 3x3 RGBA8, mip_levels=2.
        //
        // Guest UMD packing:
        // - mip0 uses row_pitch_bytes (12) => 12 * 3 = 36
        // - mip1 row pitch is tight (1 * 4) => 4 * 1 = 4
        // Total = 40 bytes.
        let allocs = [AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 40,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // SRC: guest-backed 3x3 with 2 mips.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&3u32.to_le_bytes()); // width
        stream.extend_from_slice(&3u32.to_le_bytes()); // height
        stream.extend_from_slice(&2u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&12u32.to_le_bytes()); // row_pitch_bytes (mip0 only)
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DST: non-backed texture (used to trigger upload via COPY_TEXTURE2D).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&3u32.to_le_bytes()); // width
        stream.extend_from_slice(&3u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D(dst <- src) to force a guest-memory upload for SRC.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&1u32.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&3u32.to_le_bytes()); // width
        stream.extend_from_slice(&3u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x2000);
        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("expected tight mip packing to be accepted");
    });
}

#[test]
fn create_texture2d_guest_backed_array_layers_pack_mips_per_layer() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // 3x3 RGBA8, mip_levels=2, array_layers=2.
        //
        // Per-layer packing:
        // - mip0: row_pitch_bytes (12) * 3 = 36
        // - mip1: tight (1 * 4) * 1 = 4
        // layer_stride = 40
        // total = 80
        let allocs = [AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 80,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // SRC: guest-backed 3x3 with 2 mips and 2 array layers.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&3u32.to_le_bytes()); // width
        stream.extend_from_slice(&3u32.to_le_bytes()); // height
        stream.extend_from_slice(&2u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&2u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&12u32.to_le_bytes()); // row_pitch_bytes (mip0 only)
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DST: non-backed texture (used to trigger upload via COPY_TEXTURE2D).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&3u32.to_le_bytes()); // width
        stream.extend_from_slice(&3u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D(dst <- src) to force a guest-memory upload for SRC (uploads all layers).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&1u32.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&3u32.to_le_bytes()); // width
        stream.extend_from_slice(&3u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x2000);
        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("expected tight per-layer mip packing to be accepted");
    });
}

#[test]
fn create_texture2d_bc1_guest_backed_upload_succeeds() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // BC1 4x4 is exactly one 8-byte block.
        let allocs = [AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 8,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // SRC: guest-backed BC1 4x4.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::BC1RgbaUnorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&8u32.to_le_bytes()); // row_pitch_bytes (1 block row)
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DST: non-backed BC1 4x4.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::BC1RgbaUnorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D(dst <- src) to force BC upload + decompression.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&1u32.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x2000);
        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("expected BC1 create + upload to succeed");
    });
}

#[test]
fn create_texture2d_bc1_guest_backed_upload_repacks_padded_row_pitch() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // BC1 4x8: blocks_w=1, blocks_h=2, block_bytes=8.
        // Use a padded row pitch to ensure the upload path repacks into the tight layout expected
        // by the BC decompressor.
        let row_pitch_bytes = 256u32;
        let alloc_size = row_pitch_bytes as u64 * 2; // 2 block rows.
        let allocs = [AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: alloc_size,
            reserved0: 0,
        }];

        // Two BC1 blocks: top = white, bottom = black.
        let bc1_white: [u8; 8] = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];
        let bc1_black: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0];
        let mut backing = vec![0u8; alloc_size as usize];
        backing[0..8].copy_from_slice(&bc1_white);
        backing[row_pitch_bytes as usize..row_pitch_bytes as usize + 8].copy_from_slice(&bc1_black);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // SRC: guest-backed BC1 4x8.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::BC1RgbaUnorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&8u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&row_pitch_bytes.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&1u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DST: non-backed BC1 4x8.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // texture_handle
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::BC1RgbaUnorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&8u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D(dst <- src) to force BC upload + decompression.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&2u32.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&1u32.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&8u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x2000);
        guest_mem
            .write(0x100, &backing)
            .expect("write BC backing into guest memory");

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("expected BC1 upload with padded row pitch to succeed");
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(2)
            .await
            .expect("read back dst texture");
        assert_eq!(pixels.len(), (4 * 8 * 4) as usize);

        let row_stride = 4 * 4;
        for y in 0..8usize {
            let expected = if y < 4 {
                [255u8, 255u8, 255u8, 255u8]
            } else {
                [0u8, 0u8, 0u8, 255u8]
            };
            let row = &pixels[y * row_stride..(y + 1) * row_stride];
            for px in row.chunks_exact(4) {
                assert_eq!(px, &expected, "mismatch at y={y}");
            }
        }
    });
}
