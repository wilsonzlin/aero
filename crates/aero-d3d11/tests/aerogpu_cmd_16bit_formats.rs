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

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut stream = Vec::new();
    // Stream header (24 bytes)
    stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

    packets(&mut stream);

    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
    stream
}

#[test]
fn aerogpu_cmd_upload_resource_supports_16bit_packed_formats() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // ---- B5G6R5Unorm (with per-row padding) ----
        {
            const TEX: u32 = 1;
            let width = 2u32;
            let height = 2u32;
            let row_pitch_bytes = 8u32; // 4 bytes pixels + 4 bytes padding

            // 2x2 pixels, row-major:
            // row0: red, green
            // row1: blue, white
            let mut b5 = Vec::new();
            // row0
            b5.extend_from_slice(&[0x00, 0xF8, 0xE0, 0x07]);
            b5.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // padding (must be ignored)
            // row1
            b5.extend_from_slice(&[0x1F, 0x00, 0xFF, 0xFF]);
            b5.extend_from_slice(&[0xFE, 0xED, 0xFA, 0xCE]); // padding (must be ignored)
            assert_eq!(b5.len(), row_pitch_bytes as usize * height as usize);

            let stream = build_stream(|stream| {
                // CREATE_TEXTURE2D (host allocated)
                let start = begin_cmd(stream, AerogpuCmdOpcode::CreateTexture2d as u32);
                stream.extend_from_slice(&TEX.to_le_bytes());
                stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
                stream.extend_from_slice(&(AerogpuFormat::B5G6R5Unorm as u32).to_le_bytes());
                stream.extend_from_slice(&width.to_le_bytes());
                stream.extend_from_slice(&height.to_le_bytes());
                stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
                stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
                stream.extend_from_slice(&row_pitch_bytes.to_le_bytes());
                stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
                stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
                stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                end_cmd(stream, start);

                // UPLOAD_RESOURCE full texture
                let start = begin_cmd(stream, AerogpuCmdOpcode::UploadResource as u32);
                stream.extend_from_slice(&TEX.to_le_bytes());
                stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
                stream.extend_from_slice(&(b5.len() as u64).to_le_bytes());
                stream.extend_from_slice(&b5);
                stream.resize(stream.len() + (align4(b5.len()) - b5.len()), 0);
                end_cmd(stream, start);
            });

            let mut guest_mem = VecGuestMemory::new(0);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("execute_cmd_stream should succeed");
            exec.poll_wait();

            let pixels = exec
                .read_texture_rgba8(TEX)
                .await
                .expect("readback should succeed");
            assert_eq!(
                pixels,
                vec![
                    255, 0, 0, 255, // red
                    0, 255, 0, 255, // green
                    0, 0, 255, 255, // blue
                    255, 255, 255, 255, // white
                ]
            );
        }

        // ---- B5G5R5A1Unorm (alpha=0 and alpha=1) ----
        {
            const TEX: u32 = 2;
            let width = 2u32;
            let height = 2u32;

            // row0: red (a=1), green (a=0)
            // row1: blue (a=1), white (a=0)
            let b5: [u8; 8] = [
                0x00, 0xFC, // red, a=1
                0xE0, 0x03, // green, a=0
                0x1F, 0x80, // blue, a=1
                0xFF, 0x7F, // white, a=0
            ];

            let stream = build_stream(|stream| {
                // CREATE_TEXTURE2D (host allocated, tight packing)
                let start = begin_cmd(stream, AerogpuCmdOpcode::CreateTexture2d as u32);
                stream.extend_from_slice(&TEX.to_le_bytes());
                stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
                stream.extend_from_slice(&(AerogpuFormat::B5G5R5A1Unorm as u32).to_le_bytes());
                stream.extend_from_slice(&width.to_le_bytes());
                stream.extend_from_slice(&height.to_le_bytes());
                stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
                stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
                stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes (tight)
                stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
                stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
                stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                end_cmd(stream, start);

                // UPLOAD_RESOURCE full texture
                let start = begin_cmd(stream, AerogpuCmdOpcode::UploadResource as u32);
                stream.extend_from_slice(&TEX.to_le_bytes());
                stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
                stream.extend_from_slice(&(b5.len() as u64).to_le_bytes());
                stream.extend_from_slice(&b5);
                stream.resize(stream.len() + (align4(b5.len()) - b5.len()), 0);
                end_cmd(stream, start);
            });

            let mut guest_mem = VecGuestMemory::new(0);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("execute_cmd_stream should succeed");
            exec.poll_wait();

            let pixels = exec
                .read_texture_rgba8(TEX)
                .await
                .expect("readback should succeed");
            assert_eq!(
                pixels,
                vec![
                    255, 0, 0, 255, // red, a=1
                    0, 255, 0, 0, // green, a=0
                    0, 0, 255, 255, // blue, a=1
                    255, 255, 255, 0, // white, a=0
                ]
            );
        }
    });
}

#[test]
fn aerogpu_cmd_guest_backed_b5_formats_expand_on_upload_and_copy() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

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
        let row_pitch_bytes = 6u32; // 4 bytes pixels + 2 bytes padding per row
        let src_bytes_len = (row_pitch_bytes * height) as usize;

        const SRC_565: u32 = 1;
        const DST_565: u32 = 2;
        const SRC_5551: u32 = 3;
        const DST_5551: u32 = 4;

        let src_565_offset = 0u32;
        let src_5551_offset = 0x100u32;

        let mut guest_mem = VecGuestMemory::new(0x2000);

        // B5G6R5Unorm guest bytes with padding.
        let mut src_565 = Vec::new();
        // row0: red, green
        src_565.extend_from_slice(&[0x00, 0xF8, 0xE0, 0x07]);
        src_565.extend_from_slice(&[0xDE, 0xAD]); // padding
        // row1: blue, white
        src_565.extend_from_slice(&[0x1F, 0x00, 0xFF, 0xFF]);
        src_565.extend_from_slice(&[0xBE, 0xEF]); // padding
        assert_eq!(src_565.len(), src_bytes_len);
        guest_mem
            .write(alloc.gpa + src_565_offset as u64, &src_565)
            .expect("write src565");

        // B5G5R5A1Unorm guest bytes with padding.
        let mut src_5551 = Vec::new();
        // row0: red (a=1), green (a=0)
        src_5551.extend_from_slice(&[0x00, 0xFC, 0xE0, 0x03]);
        src_5551.extend_from_slice(&[0x11, 0x22]); // padding
        // row1: blue (a=1), white (a=0)
        src_5551.extend_from_slice(&[0x1F, 0x80, 0xFF, 0x7F]);
        src_5551.extend_from_slice(&[0x33, 0x44]); // padding
        assert_eq!(src_5551.len(), src_bytes_len);
        guest_mem
            .write(alloc.gpa + src_5551_offset as u64, &src_5551)
            .expect("write src5551");

        let stream = build_stream(|stream| {
            // --- B5G6R5 ---
            // CREATE_TEXTURE2D SRC (guest-backed)
            let start = begin_cmd(stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&SRC_565.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::B5G6R5Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&row_pitch_bytes.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&src_565_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(stream, start);

            // CREATE_TEXTURE2D DST (host-owned)
            let start = begin_cmd(stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&DST_565.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::B5G6R5Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes (tight)
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(stream, start);

            // RESOURCE_DIRTY_RANGE src
            let start = begin_cmd(stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
            stream.extend_from_slice(&SRC_565.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&(src_bytes_len as u64).to_le_bytes());
            end_cmd(stream, start);

            // COPY_TEXTURE2D SRC -> DST (forces upload of guest-backed SRC)
            let start = begin_cmd(stream, AerogpuCmdOpcode::CopyTexture2d as u32);
            stream.extend_from_slice(&DST_565.to_le_bytes());
            stream.extend_from_slice(&SRC_565.to_le_bytes());
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
            end_cmd(stream, start);

            // --- B5G5R5A1 ---
            // CREATE_TEXTURE2D SRC (guest-backed)
            let start = begin_cmd(stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&SRC_5551.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::B5G5R5A1Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&row_pitch_bytes.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&src_5551_offset.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(stream, start);

            // CREATE_TEXTURE2D DST (host-owned)
            let start = begin_cmd(stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&DST_5551.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::B5G5R5A1Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&width.to_le_bytes());
            stream.extend_from_slice(&height.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes (tight)
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(stream, start);

            // RESOURCE_DIRTY_RANGE src
            let start = begin_cmd(stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
            stream.extend_from_slice(&SRC_5551.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&(src_bytes_len as u64).to_le_bytes());
            end_cmd(stream, start);

            // COPY_TEXTURE2D SRC -> DST (forces upload of guest-backed SRC)
            let start = begin_cmd(stream, AerogpuCmdOpcode::CopyTexture2d as u32);
            stream.extend_from_slice(&DST_5551.to_le_bytes());
            stream.extend_from_slice(&SRC_5551.to_le_bytes());
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
            end_cmd(stream, start);
        });

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels_565 = exec
            .read_texture_rgba8(DST_565)
            .await
            .expect("readback 565");
        assert_eq!(
            pixels_565,
            vec![
                255, 0, 0, 255, // red
                0, 255, 0, 255, // green
                0, 0, 255, 255, // blue
                255, 255, 255, 255, // white
            ]
        );

        let pixels_5551 = exec
            .read_texture_rgba8(DST_5551)
            .await
            .expect("readback 5551");
        assert_eq!(
            pixels_5551,
            vec![
                255, 0, 0, 255, // red, a=1
                0, 255, 0, 0, // green, a=0
                0, 0, 255, 255, // blue, a=1
                255, 255, 255, 0, // white, a=0
            ]
        );
    });
}
