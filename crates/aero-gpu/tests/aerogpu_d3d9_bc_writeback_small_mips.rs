#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::stats::GpuStats;
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
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

async fn create_executor_with_bc_features() -> Option<AerogpuD3d9Executor> {
    common::ensure_xdg_runtime_dir();

    if env_var_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
        return None;
    }

    async fn try_create(backends: wgpu::Backends) -> Option<AerogpuD3d9Executor> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        // Avoid CPU adapters on Linux for native BC paths.
        let disallow_cpu = cfg!(target_os = "linux");
        for adapter in instance.enumerate_adapters(backends) {
            if !adapter
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
            {
                continue;
            }
            let info = adapter.get_info();
            if disallow_cpu && info.device_type == wgpu::DeviceType::Cpu {
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
                        label: Some("aerogpu d3d9 bc writeback test device"),
                        required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                        required_limits,
                    },
                    None,
                )
                .await
            else {
                continue;
            };

            // Turn any wgpu validation errors into hard test failures.
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

    // Avoid wgpu's GL backend on Linux: wgpu-hal's GLES pipeline reflection can panic for some
    // shader pipelines (observed in CI sandboxes), which turns these tests into hard failures.
    if cfg!(target_os = "linux") {
        if let Some(exec) = try_create(wgpu::Backends::PRIMARY).await {
            return Some(exec);
        }
    }

    let backends = if cfg!(target_os = "linux") {
        wgpu::Backends::all() - wgpu::Backends::GL
    } else {
        wgpu::Backends::all()
    };
    try_create(backends).await
}

#[test]
fn d3d9_bc_writeback_pads_small_mips() {
    const TEST_NAME: &str = concat!(module_path!(), "::d3d9_bc_writeback_pads_small_mips");

    let mut exec = match pollster::block_on(create_executor_with_bc_features()) {
        Some(exec) => exec,
        None => {
            common::skip_or_panic(
                TEST_NAME,
                "TEXTURE_COMPRESSION_BC not available (or disabled via AERO_DISABLE_WGPU_TEXTURE_COMPRESSION)",
            );
            return;
        }
    };

    const DST_TEX: u32 = 1;
    const SRC_TEX: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const SRC_ALLOC_ID: u32 = 2;
    const DST_GPA: u64 = 0x1000;
    const SRC_GPA: u64 = 0x2000;

    let mip0_size_bytes: usize = 8; // 4x4 BC1 mip0 = 1 block
    let mip1_size_bytes: usize = 8; // 2x2 BC1 mip1 = 1 block
    let backing_size_bytes: usize = mip0_size_bytes + mip1_size_bytes;

    let mut guest = VecGuestMemory::new(0x4000);
    guest
        .write(DST_GPA, &vec![0xAAu8; backing_size_bytes])
        .unwrap();

    let mut src_backing = Vec::new();
    src_backing.extend_from_slice(&[0u8; 8]); // mip0
    src_backing.extend_from_slice(&[0x5Au8; 8]); // mip1
    guest.write(SRC_GPA, &src_backing).unwrap();

    let alloc_table = AllocTable::new([
        (
            DST_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: DST_GPA,
                size_bytes: backing_size_bytes as u64,
            },
        ),
        (
            SRC_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: SRC_GPA,
                size_bytes: backing_size_bytes as u64,
            },
        ),
    ])
    .expect("alloc table");

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        DST_TEX,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::BC1RgbaUnorm as u32,
        4,
        4,
        2,
        1,
        8, // row_pitch_bytes (mip0 block row)
        DST_ALLOC_ID,
        0,
    );
    writer.create_texture2d(
        SRC_TEX,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::BC1RgbaUnorm as u32,
        4,
        4,
        2,
        1,
        8, // row_pitch_bytes (mip0 block row)
        SRC_ALLOC_ID,
        0,
    );
    // Mark mip1 as dirty so the executor uploads it from guest memory (and must use a 4x4
    // physical extent on wgpu).
    writer.resource_dirty_range(SRC_TEX, mip0_size_bytes as u64, mip1_size_bytes as u64);
    // Copy mip1 (2x2 logical) and write it back into the destination guest backing.
    writer.copy_texture2d_writeback_dst(DST_TEX, SRC_TEX, 1, 0, 1, 0, 0, 0, 0, 0, 2, 2);
    let stream = writer.finish();

    match exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest, Some(&alloc_table)) {
        Ok(()) => {}
        Err(AerogpuD3d9Error::Validation(msg))
            if msg.contains("WRITEBACK_DST is not supported for BC textures") =>
        {
            common::skip_or_panic(TEST_NAME, &msg);
            return;
        }
        Err(err) => panic!("execution failed: {err}"),
    }

    // mip0 should be unchanged.
    let mut mip0 = vec![0u8; mip0_size_bytes];
    guest.read(DST_GPA, &mut mip0).unwrap();
    assert_eq!(mip0, vec![0xAAu8; mip0_size_bytes]);

    // mip1 should reflect the written back block.
    let mut mip1 = vec![0u8; mip1_size_bytes];
    guest
        .read(DST_GPA + mip0_size_bytes as u64, &mut mip1)
        .unwrap();
    assert_eq!(mip1, vec![0x5Au8; mip1_size_bytes]);
}
