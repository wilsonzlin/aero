mod common;

use aero_d3d11::runtime::aerogpu_resources::AerogpuResourceManager;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::FourCC;
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use anyhow::{anyhow, Result};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    let version = ((stage_type as u32) << 16) | (5u32 << 4);
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue)> {
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

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::PRIMARY
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
    }
    .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 compute shader create test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue))
}

#[test]
fn create_shader_dxbc_compute_uses_signature_driven_translation_even_without_osgn() -> Result<()> {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("{err:#}"));
                return Ok(());
            }
        };
        let mut mgr = AerogpuResourceManager::new(device, queue);

        // Minimal SM5 compute program with no signature chunks. Compute shaders require a thread
        // group size declaration, so include `dcl_thread_group 1, 1, 1`.
        let body = [
            opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
            1,
            1,
            1,
            opcode_token(OPCODE_RET, 1),
        ];
        let tokens = make_sm5_program_tokens(5 /* compute */, &body);
        let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);

        let handle = 1u32;
        mgr.create_shader_dxbc(handle, AerogpuShaderStage::Compute as u32, &dxbc_bytes)?;

        let shader = mgr.shader(handle)?;
        assert!(
            shader.wgsl.contains("@compute"),
            "expected WGSL to contain @compute:\n{}",
            shader.wgsl
        );
        assert!(
            shader.wgsl.contains("cs_main"),
            "expected WGSL to contain cs_main entry point:\n{}",
            shader.wgsl
        );
        Ok(())
    })
}
