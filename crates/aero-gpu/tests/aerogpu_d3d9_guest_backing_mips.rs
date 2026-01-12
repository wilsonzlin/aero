mod common;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn d3d9_guest_backed_mip_chain_dirty_range_uploads_all_subresources() {
    common::ensure_xdg_runtime_dir();

    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const SRC_HANDLE: u32 = 1;
    const DST_HANDLE: u32 = 2;

    const TEX_ALLOC_ID: u32 = 1;
    const TEX_GPA: u64 = 0x1000;

    let width = 4u32;
    let height = 4u32;
    let mip_levels = 2u32;
    let array_layers = 1u32;
    let bpp = 4u32;

    let mip0_size = (width * height * bpp) as usize; // 4x4 RGBA8
    let mip1_w = 2usize;
    let mip1_h = 2usize;
    let mip1_size = mip1_w * mip1_h * bpp as usize; // 2x2 RGBA8

    // Backing layout: mip0 then mip1, tightly packed.
    let mut backing = Vec::with_capacity(mip0_size + mip1_size);
    backing.extend(std::iter::repeat_n(0x11u8, mip0_size)); // arbitrary mip0 contents
    for _ in 0..(mip1_w * mip1_h) {
        // green
        backing.extend_from_slice(&[0, 255, 0, 255]);
    }

    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory.write(TEX_GPA, &backing).unwrap();

    let alloc_table = AllocTable::new([(
        TEX_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: TEX_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let mip1_offset_bytes = mip0_size as u64;
    let mip1_size_bytes = mip1_size as u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        SRC_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        width,
        height,
        mip_levels,
        array_layers,
        width * bpp, // row_pitch_bytes (tight)
        TEX_ALLOC_ID,
        0, // backing_offset_bytes
    );

    // Destination is a host-owned 2x2 texture.
    writer.create_texture2d(
        DST_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        2,
        2,
        1,
        1,
        0, // row_pitch_bytes (tight)
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );

    // Only mip1 is marked dirty.
    writer.resource_dirty_range(SRC_HANDLE, mip1_offset_bytes, mip1_size_bytes);

    // Copy from src mip1 to dst mip0.
    writer.copy_texture2d(
        DST_HANDLE, SRC_HANDLE, 0, // dst_mip_level
        0, // dst_array_layer
        1, // src_mip_level
        0, // src_array_layer
        0, // dst_x
        0, // dst_y
        0, // src_x
        0, // src_y
        2, // width
        2, // height
        0, // flags
    );

    let stream = writer.finish();
    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(DST_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (2, 2));

    let mut expected = Vec::new();
    for _ in 0..4 {
        expected.extend_from_slice(&[0, 255, 0, 255]);
    }
    assert_eq!(rgba, expected);
}
