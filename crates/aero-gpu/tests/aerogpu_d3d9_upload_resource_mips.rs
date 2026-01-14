mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn d3d9_upload_resource_supports_mip_offsets_for_host_backed_textures() {
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

    let mip0_w = 4u32;
    let mip0_h = 4u32;
    let mip_levels = 2u32;
    let array_layers = 1u32;
    let bpp = 4u32;

    let mip0_row_pitch = mip0_w * bpp; // tight
    let mip0_size = (mip0_row_pitch * mip0_h) as u64;

    let mip1_w = 2u32;
    let mip1_h = 2u32;
    let mip1_row_pitch = mip1_w * bpp;
    let mip1_size = (mip1_row_pitch * mip1_h) as u64;

    // Linear layout: mip0 then mip1.
    let mip1_offset_bytes = mip0_size;

    let mut mip1_rgba = Vec::new();
    for _ in 0..(mip1_w * mip1_h) {
        mip1_rgba.extend_from_slice(&[0, 255, 0, 255]);
    }
    assert_eq!(mip1_rgba.len(), mip1_size as usize);

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        SRC_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        mip0_w,
        mip0_h,
        mip_levels,
        array_layers,
        mip0_row_pitch,
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );
    writer.create_texture2d(
        DST_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        mip1_w,
        mip1_h,
        1,
        1,
        0, // row_pitch_bytes
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );

    writer.upload_resource(SRC_HANDLE, mip1_offset_bytes, &mip1_rgba);
    writer.copy_texture2d(
        DST_HANDLE, // dst_texture
        SRC_HANDLE, // src_texture
        0,          // dst_mip_level
        0,          // dst_array_layer
        1,          // src_mip_level
        0,          // src_array_layer
        0,          // dst_x
        0,          // dst_y
        0,          // src_x
        0,          // src_y
        mip1_w,     // width
        mip1_h,     // height
        0,          // flags
    );

    exec.execute_cmd_stream(&writer.finish())
        .expect("mip upload + copy should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(DST_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (mip1_w, mip1_h));
    assert_eq!(rgba, mip1_rgba);
}

#[test]
fn d3d9_upload_resource_supports_cube_layer_and_mip_offsets() {
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

    let mip0_w = 4u32;
    let mip0_h = 4u32;
    let mip_levels = 2u32;
    let array_layers = 6u32; // cube texture
    let bpp = 4u32;

    let mip0_row_pitch = mip0_w * bpp; // tight
    let mip0_size = (mip0_row_pitch * mip0_h) as u64;

    let mip1_w = 2u32;
    let mip1_h = 2u32;
    let mip1_row_pitch = mip1_w * bpp;
    let mip1_size = (mip1_row_pitch * mip1_h) as u64;

    // Linear layout: for each layer, mip0 then mip1.
    let layer_stride = mip0_size + mip1_size;
    let src_layer = 2u64;
    let mip1_offset_bytes = src_layer * layer_stride + mip0_size;

    let mut mip1_rgba = Vec::new();
    for _ in 0..(mip1_w * mip1_h) {
        // blue
        mip1_rgba.extend_from_slice(&[0, 0, 255, 255]);
    }
    assert_eq!(mip1_rgba.len(), mip1_size as usize);

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        SRC_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        mip0_w,
        mip0_h,
        mip_levels,
        array_layers,
        mip0_row_pitch,
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );
    writer.create_texture2d(
        DST_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        mip1_w,
        mip1_h,
        1,
        1,
        0, // row_pitch_bytes
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );

    writer.upload_resource(SRC_HANDLE, mip1_offset_bytes, &mip1_rgba);
    // Copy from src mip1/layer2 to dst mip0/layer0.
    writer.copy_texture2d(
        DST_HANDLE, // dst_texture
        SRC_HANDLE, // src_texture
        0,          // dst_mip_level
        0,          // dst_array_layer
        1,          // src_mip_level
        src_layer as u32,
        0,      // dst_x
        0,      // dst_y
        0,      // src_x
        0,      // src_y
        mip1_w, // width
        mip1_h, // height
        0,      // flags
    );

    exec.execute_cmd_stream(&writer.finish())
        .expect("cube mip upload + copy should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(DST_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (mip1_w, mip1_h));
    assert_eq!(rgba, mip1_rgba);
}

#[test]
fn d3d9_upload_resource_uses_padded_mip0_row_pitch_for_mip_offsets() {
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

    let mip0_w = 4u32;
    let mip0_h = 4u32;
    let mip_levels = 2u32;
    let array_layers = 1u32;
    let bpp = 4u32;

    // Simulate a D3D9-style padded pitch for mip0 (must still be a multiple of bytes-per-texel).
    let mip0_row_pitch = 20u32;
    assert!(mip0_row_pitch >= mip0_w * bpp);
    let mip0_size = (mip0_row_pitch * mip0_h) as u64;

    let mip1_w = 2u32;
    let mip1_h = 2u32;
    let mip1_size = (mip1_w * bpp * mip1_h) as u64;

    // Linear layout uses mip0 pitch * height for the mip1 base offset.
    let mip1_offset_bytes = mip0_size;

    let mut mip1_rgba = Vec::new();
    for _ in 0..(mip1_w * mip1_h) {
        mip1_rgba.extend_from_slice(&[0, 255, 0, 255]);
    }
    assert_eq!(mip1_rgba.len(), mip1_size as usize);

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        SRC_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        mip0_w,
        mip0_h,
        mip_levels,
        array_layers,
        mip0_row_pitch, // padded pitch
        0,              // backing_alloc_id
        0,              // backing_offset_bytes
    );
    writer.create_texture2d(
        DST_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        mip1_w,
        mip1_h,
        1,
        1,
        0, // row_pitch_bytes
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );

    writer.upload_resource(SRC_HANDLE, mip1_offset_bytes, &mip1_rgba);
    writer.copy_texture2d(
        DST_HANDLE,
        SRC_HANDLE,
        0, // dst_mip_level
        0, // dst_array_layer
        1, // src_mip_level
        0, // src_array_layer
        0,
        0,
        0,
        0,
        mip1_w,
        mip1_h,
        0,
    );

    exec.execute_cmd_stream(&writer.finish())
        .expect("padded mip0 pitch mip upload + copy should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(DST_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (mip1_w, mip1_h));
    assert_eq!(rgba, mip1_rgba);
}
