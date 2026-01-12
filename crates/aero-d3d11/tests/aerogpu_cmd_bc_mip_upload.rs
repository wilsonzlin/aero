mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_RESOURCE_USAGE_TEXTURE,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn begin_cmd(stream: &mut Vec<u8>, opcode: u32) -> usize {
    let start = stream.len();
    stream.extend_from_slice(&opcode.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size placeholder
    start
}

fn end_cmd(stream: &mut [u8], start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

async fn create_executor_with_bc_features() -> Option<AerogpuD3d11Executor> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir =
                std::env::temp_dir().join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

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

    if !adapter
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return None;
    }

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd bc mip upload test device"),
                required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()?;

    Some(AerogpuD3d11Executor::new(device, queue))
}

#[test]
fn d3d11_bc_mip_upload_and_copy_pad_small_mips() {
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

    let allocs = [AerogpuAllocEntry {
        alloc_id: ALLOC_ID,
        flags: 0,
        gpa: GPA,
        size_bytes: 0x1000,
        reserved0: 0,
    }];

    // Guest layout for a 4x4 BC1 texture with 2 mips (4x4, 2x2) is:
    // - mip0: 1 BC1 block = 8 bytes
    // - mip1: 1 BC1 block = 8 bytes
    // total: 16 bytes
    let mut guest_bytes = vec![0u8; 16];
    guest_bytes[..8].copy_from_slice(&[0xAA; 8]);
    guest_bytes[8..].copy_from_slice(&[0x55; 8]);

    let mut guest_mem = VecGuestMemory::new(0x4000);
    guest_mem
        .write(GPA, &guest_bytes)
        .expect("write BC mip chain into guest memory");

    let mut stream = Vec::new();
    // Stream header (24 bytes)
    stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

    // SRC: guest-backed BC1 4x4 with mip_levels=2.
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&SRC_TEX.to_le_bytes()); // texture_handle
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
    stream.extend_from_slice(&(AerogpuFormat::BC1RgbaUnorm as u32).to_le_bytes());
    stream.extend_from_slice(&4u32.to_le_bytes()); // width
    stream.extend_from_slice(&4u32.to_le_bytes()); // height
    stream.extend_from_slice(&2u32.to_le_bytes()); // mip_levels
    stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
    stream.extend_from_slice(&8u32.to_le_bytes()); // row_pitch_bytes (mip0)
    stream.extend_from_slice(&ALLOC_ID.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // DST: non-backed BC1 4x4 with mip_levels=2.
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&DST_TEX.to_le_bytes()); // texture_handle
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
    stream.extend_from_slice(&(AerogpuFormat::BC1RgbaUnorm as u32).to_le_bytes());
    stream.extend_from_slice(&4u32.to_le_bytes()); // width
    stream.extend_from_slice(&4u32.to_le_bytes()); // height
    stream.extend_from_slice(&2u32.to_le_bytes()); // mip_levels
    stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
    stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // COPY_TEXTURE2D(dst mip1 <- src mip1) (2x2 region reaches the edge; WebGPU requires a 4x4
    // physical copy for BC formats).
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
    stream.extend_from_slice(&DST_TEX.to_le_bytes()); // dst_texture
    stream.extend_from_slice(&SRC_TEX.to_le_bytes()); // src_texture
    stream.extend_from_slice(&1u32.to_le_bytes()); // dst_mip_level
    stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
    stream.extend_from_slice(&1u32.to_le_bytes()); // src_mip_level
    stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
    stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
    stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
    stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
    stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
    stream.extend_from_slice(&2u32.to_le_bytes()); // width
    stream.extend_from_slice(&2u32.to_le_bytes()); // height
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // Patch stream size in header.
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());

    exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
        .expect("BC mip upload + small-mip copy should succeed");
    exec.poll_wait();
}

