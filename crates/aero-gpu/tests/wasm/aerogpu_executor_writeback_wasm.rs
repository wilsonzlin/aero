#![cfg(target_arch = "wasm32")]

use crate::common;
use aero_gpu::aerogpu_executor::{AeroGpuExecutor, ExecutorEvent};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

async fn create_device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await?;

    adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-gpu wasm writeback pre-scan test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()
}

#[wasm_bindgen_test(async)]
async fn aerogpu_executor_sync_rejects_writeback_before_executing_any_cmds_on_wasm() {
    let (device, queue) = match create_device_queue().await {
        Some(v) => v,
        None => {
            common::skip_or_panic(module_path!(), "wgpu adapter/device unavailable");
            return;
        }
    };

    let mut exec = AeroGpuExecutor::new(device, queue).expect("create AeroGpuExecutor");
    let mut guest_mem = VecGuestMemory::new(0x1000);

    const TEX: u32 = 1;
    const BUF_SRC: u32 = 2;
    const BUF_DST: u32 = 3;

    // If this stream were partially executed, the CREATE_* commands would allocate resources
    // before the WRITEBACK_DST validation error is observed.
    let stream = {
        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            TEX,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            0,
            0,
            0,
        );
        writer.create_buffer(BUF_SRC, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 4, 0, 0);
        writer.create_buffer(BUF_DST, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 4, 0, 0);
        writer.copy_buffer_writeback_dst(BUF_DST, BUF_SRC, 0, 0, 4);
        writer.finish()
    };

    let err = exec
        .execute_cmd_stream(&stream, &mut guest_mem, None)
        .expect_err("sync executor must reject WRITEBACK_DST on wasm");
    assert!(
        err.to_string()
            .contains("WRITEBACK_DST requires async execution on wasm"),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("first WRITEBACK_DST at packet 3"),
        "expected error to include the offending packet index, got: {err}"
    );
    assert!(
        exec.texture(TEX).is_none(),
        "expected sync rejection to happen before CREATE_TEXTURE2D executes"
    );
}

#[wasm_bindgen_test(async)]
async fn aerogpu_executor_process_cmd_stream_rejects_writeback_before_executing_on_wasm() {
    let (device, queue) = match create_device_queue().await {
        Some(v) => v,
        None => {
            common::skip_or_panic(module_path!(), "wgpu adapter/device unavailable");
            return;
        }
    };

    let mut exec = AeroGpuExecutor::new(device, queue).expect("create AeroGpuExecutor");
    let mut guest_mem = VecGuestMemory::new(0x1000);

    const TEX: u32 = 1;
    const BUF_SRC: u32 = 2;
    const BUF_DST: u32 = 3;

    let stream = {
        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            TEX,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            0,
            0,
            0,
        );
        writer.create_buffer(BUF_SRC, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 4, 0, 0);
        writer.create_buffer(BUF_DST, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 4, 0, 0);
        writer.copy_buffer_writeback_dst(BUF_DST, BUF_SRC, 0, 0, 4);
        writer.finish()
    };

    let report = exec.process_cmd_stream(&stream, &mut guest_mem, None);
    assert!(
        !report.is_ok(),
        "expected writeback pre-scan error, got: {report:?}"
    );
    assert_eq!(report.packets_processed, 0);
    let err = report.events.iter().find_map(|e| match e {
        ExecutorEvent::Error { at, message } => Some((*at, message)),
    });
    let Some((at, message)) = err else {
        panic!(
            "expected WRITEBACK_DST wasm validation error, got: {:#?}",
            report.events
        );
    };
    assert_eq!(at, 3, "expected first WRITEBACK_DST command at packet 3");
    assert!(
        message.contains("WRITEBACK_DST requires async execution on wasm"),
        "unexpected error message: {message}"
    );
    assert!(
        message.contains("first WRITEBACK_DST at packet 3"),
        "expected error to include the offending packet index, got: {message}"
    );
    assert!(
        exec.texture(TEX).is_none(),
        "expected writeback pre-scan to reject before CREATE_TEXTURE2D executes"
    );
}
