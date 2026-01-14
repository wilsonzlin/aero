mod common;

use aero_gpu::{GpuBackendKind, GpuCapabilities, GpuProfiler, GpuProfilerConfig};

#[test]
fn gpu_profiler_reports_gpu_time_when_supported_otherwise_falls_back() {
    common::ensure_xdg_runtime_dir();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            // Avoid wgpu's GL backend on Linux: wgpu-hal's GLES pipeline reflection can panic for
            // some shader pipelines (observed in CI sandboxes), which turns these tests into hard
            // failures.
            wgpu::Backends::PRIMARY
        } else {
            // Prefer "native" backends; this avoids noisy platform warnings from
            // initializing GL/WAYLAND stacks in headless CI environments.
            wgpu::Backends::PRIMARY
        },
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    }));
    let Some(adapter) = adapter else {
        common::skip_or_panic(module_path!(), "no wgpu adapter available");
        return;
    };

    let adapter_features = adapter.features();
    let requested_features = if adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY)
        && adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
    {
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
    } else {
        wgpu::Features::empty()
    };

    let (device, queue) = match pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-gpu profiler integration test"),
            required_features: requested_features,
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    )) {
        Ok(pair) => pair,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("request_device failed: {err}"));
            return;
        }
    };

    let supports_timestamp_query =
        GpuCapabilities::from_device(&device).supports_timestamp_queries();

    let mut profiler = GpuProfiler::new_wgpu_with_config(
        GpuBackendKind::WebGpu,
        &device,
        &queue,
        GpuProfilerConfig {
            query_history_frames: 2,
            readback_interval_frames: 1,
        },
    );

    // Drive enough frames to:
    // 1) fill the query ring, and
    // 2) execute a follow-up `begin_frame()` that can process the async map callback.
    for _ in 0..4 {
        profiler.begin_frame(Some(&device), Some(&queue));
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        profiler.mark_upload_start(&mut encoder);
        profiler.mark_upload_end(&mut encoder);
        profiler.mark_render_pass_start(&mut encoder);
        profiler.mark_render_pass_end(&mut encoder);
        profiler.end_encode(&mut encoder);

        let cmd_buf = encoder.finish();
        profiler.submit(&queue, cmd_buf);
    }

    // Ensure the final readback (if any) is completed and processed.
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);

    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);
    profiler.poll(&device);

    let report = profiler.get_frame_timings().expect("expected timings");
    if supports_timestamp_query {
        assert!(
            report.gpu_us.is_some(),
            "expected gpu timings with timestamp query support"
        );
    } else {
        assert!(
            report.gpu_us.is_none(),
            "expected cpu-only timings without timestamp query support"
        );
    }
}
