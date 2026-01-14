mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn d3d9_cmd_stream_clear_accepts_leading_render_target_gap() {
    common::ensure_xdg_runtime_dir();

    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT1_HANDLE: u32 = 1;
    let width = 4u32;
    let height = 4u32;

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        RT1_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        width,
        height,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ width * 4,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );

    // Bind a gapped render target array: RTV0=NULL, RTV1=RT1_HANDLE.
    writer.set_render_targets(&[0, RT1_HANDLE], /*depth_stencil=*/ 0);
    writer.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
    writer.set_scissor(0, 0, width as i32, height as i32);

    writer.clear(
        AEROGPU_CLEAR_COLOR,
        /*color_rgba=*/ [0.0, 1.0, 0.0, 1.0],
        /*depth=*/ 1.0,
        /*stencil=*/ 0,
    );

    exec.execute_cmd_stream(&writer.finish())
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT1_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    for px in rgba.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}
