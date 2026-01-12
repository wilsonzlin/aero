use aero_d3d9::runtime::{ColorFormat, D3D9Runtime, RuntimeConfig, RuntimeError, SwapChainDesc};

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
fn d3d9_runtime_create_swap_chain_rejects_zero_dimensions() {
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

        let err = rt
            .create_swap_chain(
                1,
                SwapChainDesc {
                    width: 0,
                    height: 1,
                    format: ColorFormat::Rgba8Unorm,
                },
            )
            .expect_err("expected create_swap_chain to reject width=0");
        assert!(
            matches!(err, RuntimeError::Validation(ref msg) if msg.contains("width/height")),
            "unexpected error: {err:?}"
        );
    });
}
