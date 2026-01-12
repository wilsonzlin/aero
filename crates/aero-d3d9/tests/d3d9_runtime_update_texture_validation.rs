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
fn d3d9_runtime_write_texture_rejects_out_of_range_mip_level() {
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

        rt.create_texture(
            1,
            TextureDesc {
                width: 4,
                height: 4,
                mip_level_count: 1,
                format: TextureFormat::Color(ColorFormat::Rgba8Unorm),
                usage: 0,
            },
        )
        .unwrap();

        let err = rt
            .write_texture_full_mip(1, 1, 1, 1, &[0u8; 4])
            .expect_err("expected out-of-range mip_level to be rejected");
        assert!(
            matches!(err, RuntimeError::Validation(ref msg) if msg.contains("mip_level")),
            "unexpected error: {err:?}"
        );
    });
}

