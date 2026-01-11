use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn d3d9_create_texture2d_rejects_zero_dimensions() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            eprintln!("skipping resource validation test: wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        0,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with width=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("width/height")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_zero_mip_levels() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            eprintln!("skipping resource validation test: wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        0,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with mip_levels=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("mip_levels/array_layers")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
#[test]
fn d3d9_create_texture2d_rejects_guest_backed_row_pitch_too_small() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            eprintln!("skipping resource validation test: wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    let guest_memory = VecGuestMemory::new(0x1000);
    let alloc_table = AllocTable::new([(
        1,
        AllocEntry {
            gpa: 0,
            size_bytes: 0x1000,
        },
    )]);

    // width=2 => required row_pitch is 8 bytes for RGBA8, but we pass 4.
    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        2,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        4,                                   // row_pitch_bytes (too small)
        1,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream_with_guest_memory(&stream, &guest_memory, Some(&alloc_table)) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with invalid row_pitch_bytes to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("row_pitch_bytes")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
