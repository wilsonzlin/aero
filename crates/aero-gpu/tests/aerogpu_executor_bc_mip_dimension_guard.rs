mod common;

use aero_gpu::aerogpu_executor::AeroGpuExecutor;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
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

async fn create_device_queue_with_bc_features_non_gl() -> Option<(wgpu::Device, wgpu::Queue)> {
    common::ensure_xdg_runtime_dir();

    if texture_compression_disabled_by_env() {
        return None;
    }

    let backends = wgpu::Backends::all() - wgpu::Backends::GL;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });

    // Find an adapter that supports native BC sampling and isn't using the GL backend.
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

        let Ok((device, queue)) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aerogpu executor bc mip guard test device"),
                    required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
        else {
            continue;
        };

        return Some((device, queue));
    }

    None
}

#[test]
fn executor_bc_mip_dimension_guard_falls_back_instead_of_triggering_wgpu_validation() {
    let (device, queue) = match pollster::block_on(create_device_queue_with_bc_features_non_gl()) {
        Some(v) => v,
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

    device.on_uncaptured_error(Box::new(|err| {
        panic!("wgpu uncaptured error: {err}");
    }));

    let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
    let mut guest_memory = VecGuestMemory::new(0x1000);

    // Base level is block-aligned (12x12), but mip1 is 6x6 which is >= 4 and not block-aligned.
    // wgpu/WebGPU reject creating native BC textures with such mip chains, so the executor should
    // fall back to RGBA8 to stay robust.
    const TEX: u32 = 1;

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        TEX,
        cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::BC1RgbaUnorm as u32,
        12,
        12,
        2, // mip_levels
        1, // array_layers
        0, // row_pitch_bytes
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );
    let stream = writer.finish();

    let report = exec.process_cmd_stream(&stream, &mut guest_memory, None);
    assert!(report.is_ok(), "report had errors: {:#?}", report.events);
}
