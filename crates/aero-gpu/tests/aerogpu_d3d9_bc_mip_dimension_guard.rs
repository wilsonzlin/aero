#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;

use aero_gpu::stats::GpuStats;
use aero_gpu::AerogpuD3d9Executor;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn env_var_truthy(name: &str) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

fn texture_compression_disabled_by_env() -> bool {
    env_var_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION")
}

async fn create_executor_with_bc_features_non_gl() -> Option<AerogpuD3d9Executor> {
    common::ensure_xdg_runtime_dir();

    if texture_compression_disabled_by_env() {
        return None;
    }

    let backends = wgpu::Backends::all() - wgpu::Backends::GL;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });

    // Find an adapter that supports BC and isn't using the GL backend (which is typically the
    // path that lacks BC compression support).
    //
    // Note: on Linux CI we often only have software adapters (e.g. llvmpipe), which can be flaky
    // for native BC paths. Prefer to skip instead of producing false failures.
    let allow_software_adapter = !cfg!(target_os = "linux");
    for adapter in instance.enumerate_adapters(backends) {
        let info = adapter.get_info();
        if info.backend == wgpu::Backend::Gl {
            continue;
        }
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            continue;
        }
        if !allow_software_adapter && info.device_type == wgpu::DeviceType::Cpu {
            continue;
        }

        let downlevel_flags = adapter.get_downlevel_capabilities().flags;

        // The D3D9 executor's constants uniform buffer exceeds wgpu's downlevel default 16 KiB
        // binding size.
        let mut required_limits = wgpu::Limits::downlevel_defaults();
        required_limits.max_uniform_buffer_binding_size =
            required_limits.max_uniform_buffer_binding_size.max(18432);

        let Ok((device, queue)) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aerogpu d3d9 bc mip guard test device"),
                    required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                    required_limits,
                },
                None,
            )
            .await
        else {
            continue;
        };

        device.on_uncaptured_error(Box::new(|err| {
            panic!("wgpu uncaptured error: {err}");
        }));

        return Some(AerogpuD3d9Executor::new(
            device,
            queue,
            downlevel_flags,
            Arc::new(GpuStats::new()),
        ));
    }

    None
}

#[test]
fn d3d9_bc_mip_dimension_guard_falls_back_instead_of_triggering_wgpu_validation() {
    let mut exec = match pollster::block_on(create_executor_with_bc_features_non_gl()) {
        Some(exec) => exec,
        None => {
            if texture_compression_disabled_by_env() {
                common::skip_or_panic(
                    module_path!(),
                    "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set",
                );
            } else {
                common::skip_or_panic(
                    module_path!(),
                    "wgpu adapter/device with TEXTURE_COMPRESSION_BC (non-GL) not found",
                );
            }
            return;
        }
    };

    // Base level is block-aligned (12x12), but mip1 is 6x6 which is >= 4 and not block-aligned.
    // wgpu/WebGPU rejects creating native BC textures with such mip chains. The D3D9 executor
    // should fall back to RGBA8 so the existing CPU BC decompression upload path can be used.
    const TEX: u32 = 1;

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        TEX,
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        12,
        12,
        2, // mip_levels
        1, // array_layers
        0, // row_pitch_bytes
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );
    let stream = writer.finish();

    exec.execute_cmd_stream(&stream)
        .expect("creating 12x12 BC1 with 2 mips should succeed via RGBA8 fallback");

    // If the texture remained BC-compressed, RGBA8 readback would be rejected. Successful readback
    // indicates we hit the RGBA8 fallback path.
    let (w, h, bytes) = pollster::block_on(exec.readback_texture_rgba8(TEX))
        .expect("readback should succeed for RGBA8 fallback texture");
    assert_eq!((w, h), (12, 12));
    assert_eq!(bytes.len(), (12 * 12 * 4) as usize);
}
