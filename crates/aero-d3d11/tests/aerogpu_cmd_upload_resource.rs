mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_RESOURCE_USAGE_TEXTURE,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

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
fn aerogpu_cmd_upload_resource_supports_partial_texture_uploads() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const TEX: u32 = 1;
        let width = 4u32;
        let height = 4u32;
        let bytes_per_pixel = 4usize;
        let total_bytes = (width as usize) * (height as usize) * bytes_per_pixel;

        let full_data: Vec<u8> = (0u8..(total_bytes as u8)).collect();
        let patch_offset: u64 = 7;
        let patch: [u8; 3] = [0xAA, 0xBB, 0xCC];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (host allocated)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
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

        // UPLOAD_RESOURCE full texture
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(full_data.len() as u64).to_le_bytes());
        stream.extend_from_slice(&full_data);
        stream.resize(
            stream.len() + (align4(full_data.len()) - full_data.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE partial patch
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&patch_offset.to_le_bytes());
        stream.extend_from_slice(&(patch.len() as u64).to_le_bytes());
        stream.extend_from_slice(&patch);
        stream.resize(stream.len() + (align4(patch.len()) - patch.len()), 0);
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(TEX)
            .await
            .expect("readback should succeed");

        let mut expected = full_data;
        expected[patch_offset as usize..patch_offset as usize + patch.len()]
            .copy_from_slice(&patch);
        assert_eq!(pixels, expected);
    });
}

#[test]
fn aerogpu_cmd_upload_resource_supports_mip_array_texture_uploads() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const TEX: u32 = 1;
        let width = 4u32;
        let height = 4u32;
        let mip_levels = 3u32;
        let array_layers = 2u32;

        // Target subresource: mip1/layer1 (subresource index = 1 + 1*mip_levels = 4).
        let mip_level = 1u32;
        let array_layer = 1u32;

        let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);

        let mut mip_offsets = Vec::with_capacity(mip_levels as usize);
        let mut layer_stride = 0u64;
        for level in 0..mip_levels {
            mip_offsets.push(layer_stride);
            let w = mip_extent(width, level) as u64;
            let h = mip_extent(height, level) as u64;
            layer_stride += w * h * 4;
        }

        let offset = layer_stride * (array_layer as u64) + mip_offsets[mip_level as usize];
        let mip_w = mip_extent(width, mip_level);
        let mip_h = mip_extent(height, mip_level);
        let upload_size = (mip_w * mip_h * 4) as usize;
        let upload_bytes = vec![0xABu8; upload_size];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (host allocated)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&width.to_le_bytes());
        stream.extend_from_slice(&height.to_le_bytes());
        stream.extend_from_slice(&mip_levels.to_le_bytes());
        stream.extend_from_slice(&array_layers.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE for mip1/layer1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&offset.to_le_bytes());
        stream.extend_from_slice(&(upload_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&upload_bytes);
        stream.resize(
            stream.len() + (align4(upload_bytes.len()) - upload_bytes.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8_subresource(TEX, mip_level, array_layer)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels, upload_bytes);
    });
}
