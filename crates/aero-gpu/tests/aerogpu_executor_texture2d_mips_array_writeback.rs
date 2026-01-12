mod common;

use aero_gpu::aerogpu_executor::{AeroGpuExecutor, AllocEntry, AllocTable};
use aero_gpu::{GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

async fn create_device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    common::ensure_xdg_runtime_dir();

    // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
        },
        ..Default::default()
    });

    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })
        .await
    {
        Some(adapter) => Some(adapter),
        None => {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
    }?;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aerogpu executor texture2d mips+array test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()?;

    Some((device, queue))
}

#[test]
fn executor_copy_texture2d_writeback_supports_mips_and_array_layers() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        const SRC_TEX: u32 = 1;
        const DST_TEX: u32 = 2;
        const SRC_ALLOC: u32 = 1;
        const DST_ALLOC: u32 = 2;
        const SRC_GPA: u64 = 0x1000;
        const DST_GPA: u64 = 0x2000;

        let width = 2u32;
        let height = 2u32;
        let mip_levels = 2u32;
        let array_layers = 2u32;
        let row_pitch_bytes = 8u32; // mip0 row pitch (2 texels * 4 Bpp)

        // Canonical packed layout:
        // layer0 mip0: offset 0  size 16
        // layer0 mip1: offset 16 size 4
        // layer1 mip0: offset 20 size 16
        // layer1 mip1: offset 36 size 4
        let backing_size_bytes = 40usize;
        let src_dirty_off = 36u64;
        let dst_expected_off = 16u64;
        let pixel = [0x12u8, 0x34u8, 0x56u8, 0x78u8];

        let mut guest = VecGuestMemory::new(0x10_000);
        guest
            .write(SRC_GPA, &vec![0u8; backing_size_bytes])
            .unwrap();
        guest
            .write(DST_GPA, &vec![0u8; backing_size_bytes])
            .unwrap();
        guest.write(SRC_GPA + src_dirty_off, &pixel).unwrap();

        let alloc_table = AllocTable::new([
            (
                SRC_ALLOC,
                AllocEntry {
                    flags: 0,
                    gpa: SRC_GPA,
                    size_bytes: 0x1000,
                },
            ),
            (
                DST_ALLOC,
                AllocEntry {
                    flags: 0,
                    gpa: DST_GPA,
                    size_bytes: 0x1000,
                },
            ),
        ])
        .expect("alloc table");

        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            SRC_TEX,
            aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
            SRC_ALLOC,
            0,
        );
        writer.create_texture2d(
            DST_TEX,
            aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
            DST_ALLOC,
            0,
        );

        // Mark only (src mip1, layer1) dirty.
        writer.resource_dirty_range(SRC_TEX, src_dirty_off, pixel.len() as u64);

        // Copy (src mip1, layer1) -> (dst mip1, layer0) and write back the destination.
        writer.copy_texture2d(
            DST_TEX,
            SRC_TEX,
            1, // dst_mip_level
            0, // dst_array_layer
            1, // src_mip_level
            1, // src_array_layer
            0, // dst_x
            0, // dst_y
            0, // src_x
            0, // src_y
            1, // width
            1, // height
            AEROGPU_COPY_FLAG_WRITEBACK_DST,
        );

        let stream = writer.finish();
        let report = exec.process_cmd_stream(&stream, &mut guest, Some(&alloc_table));
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        // Verify the destination allocation was updated at the expected packed offset.
        let mut out = vec![0u8; backing_size_bytes];
        guest.read(DST_GPA, &mut out).unwrap();

        assert_eq!(
            &out[dst_expected_off as usize..dst_expected_off as usize + 4],
            &pixel
        );
        // Everything else stays zero.
        for (i, b) in out.into_iter().enumerate() {
            let in_expected =
                (dst_expected_off as usize..dst_expected_off as usize + 4).contains(&i);
            if in_expected {
                continue;
            }
            assert_eq!(b, 0u8, "unexpected non-zero byte at dst offset {i}");
        }
    });
}
