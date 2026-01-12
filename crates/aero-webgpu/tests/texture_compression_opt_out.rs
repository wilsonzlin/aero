use aero_webgpu::WebGpuContext;

mod common;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

// This test validates the end-to-end integration point:
// when AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is enabled, aero-webgpu must not request
// texture-compression features, so BackendCaps.texture_compression.* stays false even on adapters
// that support them.
#[test]
fn texture_compression_opt_out_disables_caps() {
    let _lock = ENV_LOCK.lock().unwrap();
    // Env vars are process-global, so use a guard to ensure we restore any pre-existing value.
    let _guard = EnvVarGuard::set("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION", "1");

    pollster::block_on(async {
        let ctx = match WebGpuContext::request_headless(Default::default()).await {
            Ok(ctx) => ctx,
            Err(err) => {
                let reason = err.to_string();
                common::skip_or_panic("texture_compression_opt_out_disables_caps", &reason);
                return;
            }
        };

        let adapter_features = ctx.adapter().features();
        let device_features = ctx.device().features();
        let caps = ctx.caps().texture_compression;

        for (name, feature, cap) in [
            (
                "TEXTURE_COMPRESSION_BC",
                wgpu::Features::TEXTURE_COMPRESSION_BC,
                caps.bc,
            ),
            (
                "TEXTURE_COMPRESSION_ETC2",
                wgpu::Features::TEXTURE_COMPRESSION_ETC2,
                caps.etc2,
            ),
            (
                "TEXTURE_COMPRESSION_ASTC_HDR",
                wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR,
                caps.astc,
            ),
        ] {
            assert!(
                !device_features.contains(feature),
                "device unexpectedly exposes {name} even though AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1"
            );
            if adapter_features.contains(feature) {
                assert!(
                    !cap,
                    "adapter supports {name} but caps reported it as enabled despite AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1"
                );
            }
        }
    });
}
