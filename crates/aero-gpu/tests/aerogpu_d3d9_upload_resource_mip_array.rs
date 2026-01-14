mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn d3d9_upload_resource_supports_deep_mip_and_array_layer_offsets() {
    common::ensure_xdg_runtime_dir();

    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    let base_w = 9u32;
    let base_h = 9u32;
    let mip_levels = 4u32;
    let array_layers = 6u32; // cube texture
    let bpp = 4u32;

    // Use a padded pitch for mip0 to ensure the offset calculation matches the guest layout.
    let mip0_row_pitch = 40u32;
    assert!(mip0_row_pitch >= base_w * bpp);
    assert!(mip0_row_pitch.is_multiple_of(bpp));

    let target_layer = 4u32;
    let target_mip = 2u32;
    let target_w = (base_w >> target_mip).max(1);
    let target_h = (base_h >> target_mip).max(1);

    // Compute the packed mip+array layout expected by the executor: for each array layer, mip0
    // uses the provided row_pitch_bytes, while subsequent mips use a tight pitch.
    let mut mip_offsets = Vec::with_capacity(mip_levels as usize);
    let mut layer_stride: u64 = 0;
    for mip in 0..mip_levels {
        mip_offsets.push(layer_stride);
        let w = (base_w >> mip).max(1);
        let h = (base_h >> mip).max(1);
        let row_pitch = if mip == 0 { mip0_row_pitch } else { w * bpp };
        layer_stride += u64::from(row_pitch) * u64::from(h);
    }
    let target_offset_in_layer = mip_offsets[target_mip as usize];
    let target_offset_bytes = u64::from(target_layer) * layer_stride + target_offset_in_layer;

    let target_size_bytes = u64::from(target_w * bpp) * u64::from(target_h);
    let mut target_bytes = vec![0u8; target_size_bytes as usize];
    for px in target_bytes.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 255, 0, 255]); // green
    }

    let mut stream = AerogpuCmdWriter::new();
    stream.create_texture2d(
        SRC_TEX,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        base_w,
        base_h,
        mip_levels,
        array_layers,
        mip0_row_pitch,
        0, // backing_alloc_id (host-owned)
        0, // backing_offset_bytes
    );
    stream.create_texture2d(
        DST_TEX,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        target_w,
        target_h,
        1,
        1,
        0,
        0,
        0,
    );

    stream.upload_resource(SRC_TEX, target_offset_bytes, &target_bytes);

    // Copy src (layer+mip) into dst mip0 so we can read back the result.
    stream.copy_texture2d(
        DST_TEX,
        SRC_TEX,
        0,
        0,
        target_mip,
        target_layer,
        0,
        0,
        0,
        0,
        target_w,
        target_h,
        0,
    );

    exec.execute_cmd_stream(&stream.finish())
        .expect("execute should succeed");

    let (out_w, out_h, rgba) =
        pollster::block_on(exec.readback_texture_rgba8(DST_TEX)).expect("readback should succeed");
    assert_eq!((out_w, out_h), (target_w, target_h));
    assert_eq!(rgba, target_bytes);
}
