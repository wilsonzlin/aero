use aero_d3d9::runtime::{ColorFormat, D3D9Runtime, RuntimeConfig, RuntimeError, TextureDesc, TextureFormat};

fn require_webgpu() -> bool {
    std::env::var("AERO_REQUIRE_WEBGPU")
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

#[test]
fn d3d9_runtime_create_texture_rejects_mip_levels_beyond_chain_length() {
    pollster::block_on(async {
        let require_webgpu = require_webgpu();
        let mut rt = match D3D9Runtime::new(RuntimeConfig::default()).await {
            Ok(rt) => rt,
            Err(err @ (RuntimeError::AdapterNotFound | RuntimeError::RequestDevice(_))) => {
                if require_webgpu {
                    panic!("AERO_REQUIRE_WEBGPU is enabled but D3D9Runtime init failed: {err}");
                }
                eprintln!("skipping WebGPU-dependent test: D3D9Runtime init failed: {err}");
                return;
            }
            Err(err) => panic!("D3D9Runtime init failed unexpectedly: {err}"),
        };

        // 4x4 textures only have 3 mip levels (4x4, 2x2, 1x1). Requesting 4 should be rejected
        // before it reaches wgpu validation.
        let err = rt
            .create_texture(
                1,
                TextureDesc {
                    width: 4,
                    height: 4,
                    mip_level_count: 4, // invalid
                    format: TextureFormat::Color(ColorFormat::Rgba8Unorm),
                    usage: 0,
                },
            )
            .expect_err("expected create_texture to reject invalid mip_level_count");

        match err {
            RuntimeError::Validation(msg) => assert!(
                msg.contains("mip_level_count"),
                "unexpected validation message: {msg}"
            ),
            other => panic!("unexpected error type: {other:?}"),
        }
    });
}

