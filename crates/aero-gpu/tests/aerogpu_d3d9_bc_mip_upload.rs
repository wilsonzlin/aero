mod common;

use std::sync::Arc;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::stats::GpuStats;
use aero_gpu::{AerogpuD3d9Executor, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

async fn create_executor_with_bc_features() -> Option<AerogpuD3d9Executor> {
    common::ensure_xdg_runtime_dir();

    // Prefer wgpu's GL backend on Linux CI for stability. Vulkan software adapters have been a
    // recurring source of flakes/crashes in headless sandboxes.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
        },
        ..Default::default()
    });

    // Avoid CPU software adapters on Linux for native BC paths; they are a common source of flakes
    // and crashes (even if they advertise TEXTURE_COMPRESSION_BC).
    let disallow_cpu = cfg!(target_os = "linux");

    // Try a couple different adapter options; the default request may land on an adapter that
    // doesn't support BC compression even when another does (e.g. integrated vs discrete).
    let adapter_opts = if disallow_cpu {
        [
            wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
            wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
        ]
    } else {
        [
            wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            },
            wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
        ]
    };

    for opts in adapter_opts {
        let Some(adapter) = instance.request_adapter(&opts).await else {
            continue;
        };
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            continue;
        }
        if disallow_cpu && adapter.get_info().device_type == wgpu::DeviceType::Cpu {
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
                    label: Some("aerogpu d3d9 bc mip upload test device"),
                    required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                    required_limits,
                },
                None,
            )
            .await
        else {
            continue;
        };

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
fn d3d9_bc_mip_dirty_range_upload_pads_small_mips() {
    let mut exec = match pollster::block_on(create_executor_with_bc_features()) {
        Some(exec) => exec,
        None => {
            common::skip_or_panic(module_path!(), "TEXTURE_COMPRESSION_BC not supported");
            return;
        }
    };

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const ALLOC_ID: u32 = 1;
    const GPA: u64 = 0x1000;

    // Guest layout for a 4x4 BC1 texture with 2 mips (4x4, 2x2) is:
    // - mip0: 1 BC1 block = 8 bytes
    // - mip1: 1 BC1 block = 8 bytes
    // total: 16 bytes
    let mut guest_bytes = vec![0u8; 16];
    // Fill each mip with different byte patterns so we have deterministic, non-zero data.
    guest_bytes[..8].copy_from_slice(&[0xAA; 8]);
    guest_bytes[8..].copy_from_slice(&[0x55; 8]);

    let alloc_table = AllocTable::new([(
        ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");
    let mut guest_memory = VecGuestMemory::new(0x2000);
    guest_memory
        .write(GPA, &guest_bytes)
        .expect("write guest BC mip chain");

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        SRC_TEX,                            // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width (block-aligned so we take the direct BC path)
        4,                                  // height
        2,                                  // mip_levels
        1,                                  // array_layers
        8,                                  // row_pitch_bytes (1 BC1 block row)
        ALLOC_ID,                           // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    writer.create_texture2d(
        DST_TEX,                            // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width
        4,                                  // height
        2,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );

    // Mark the entire backing dirty so the executor uploads mip0 and mip1 (mip1 is 2x2, which
    // requires a 4x4 physical copy for BC formats).
    writer.resource_dirty_range(SRC_TEX, 0, guest_bytes.len() as u64);

    // Trigger flushing by issuing a copy.
    writer.copy_texture2d(
        DST_TEX, // dst_texture
        SRC_TEX, // src_texture
        0,       // dst_mip_level
        0,       // dst_array_layer
        0,       // src_mip_level
        0,       // src_array_layer
        0,       // dst_x
        0,       // dst_y
        0,       // src_x
        0,       // src_y
        4,       // width
        4,       // height
        0,       // flags
    );

    let stream = writer.finish();
    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("BC mip dirty-range upload + copy should succeed");
}
