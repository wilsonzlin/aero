mod common;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AEROGPU_COPY_FLAG_WRITEBACK_DST, AEROGPU_RESOURCE_USAGE_TEXTURE,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn d3d9_copy_texture2d_writeback_dst_accounts_for_mip_offsets() {
    common::ensure_xdg_runtime_dir();

    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const DST_HANDLE: u32 = 1;
    const SRC_HANDLE: u32 = 2;

    const TEX_ALLOC_ID: u32 = 1;
    const TEX_GPA: u64 = 0x1000;

    let dst_w = 4u32;
    let dst_h = 4u32;
    let mip_levels = 2u32;
    let array_layers = 1u32;
    let bpp = 4u32;

    let mip0_size_bytes = (dst_w * dst_h * bpp) as usize;
    let mip1_size_bytes = (2u32 * 2u32 * bpp) as usize;
    let backing_size_bytes = mip0_size_bytes + mip1_size_bytes;

    let mip1_offset_bytes = mip0_size_bytes as u64;

    let mut guest_memory = VecGuestMemory::new(0x4000);
    // Initialize the entire backing to zero so the test can verify that only the intended
    // subresource gets updated.
    guest_memory
        .write(TEX_GPA, &vec![0u8; backing_size_bytes])
        .unwrap();

    let alloc_table = AllocTable::new([(
        TEX_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: TEX_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let mut src_pixels = Vec::new();
    for _ in 0..4 {
        // 2x2 green.
        src_pixels.extend_from_slice(&[0, 255, 0, 255]);
    }

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        DST_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        dst_w,
        dst_h,
        mip_levels,
        array_layers,
        dst_w * bpp, // row_pitch_bytes (tight for mip0; required for multi-subresource backing)
        TEX_ALLOC_ID,
        0, // backing_offset_bytes
    );
    writer.create_texture2d(
        SRC_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        2,
        2,
        1,
        1,
        2 * bpp, // row_pitch_bytes
        0,       // backing_alloc_id (host-owned)
        0,       // backing_offset_bytes
    );
    writer.upload_resource(SRC_HANDLE, 0, &src_pixels);
    writer.copy_texture2d(
        DST_HANDLE,
        SRC_HANDLE,
        1, // dst_mip_level
        0, // dst_array_layer
        0, // src_mip_level
        0, // src_array_layer
        0, // dst_x
        0, // dst_y
        0, // src_x
        0, // src_y
        2, // width
        2, // height
        AEROGPU_COPY_FLAG_WRITEBACK_DST,
    );

    let stream = writer.finish();
    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("execute should succeed");

    // mip0 should be unchanged (zeros).
    let mut mip0 = vec![0u8; mip0_size_bytes];
    guest_memory.read(TEX_GPA, &mut mip0).unwrap();
    assert_eq!(mip0, vec![0u8; mip0_size_bytes]);

    // mip1 should contain the written back pattern.
    let mut mip1 = vec![0u8; mip1_size_bytes];
    guest_memory
        .read(TEX_GPA + mip1_offset_bytes, &mut mip1)
        .unwrap();
    assert_eq!(mip1, src_pixels);
}

