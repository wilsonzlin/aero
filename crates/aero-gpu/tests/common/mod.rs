//! Shared helpers for `aero-gpu` integration tests.
//!
//! Note: D3D9 defaults to back-face culling with clockwise front faces
//! (`D3DCULL_CCW`). Tests that render triangles without explicitly setting cull
//! state should use clockwise vertex winding to avoid having geometry culled.

pub fn require_webgpu() -> bool {
    let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

pub fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}

#[allow(dead_code)]
pub fn ensure_xdg_runtime_dir() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::OnceLock;

        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);
            if !needs_runtime_dir {
                return;
            }

            let dir =
                std::env::temp_dir().join(format!("aero-gpu-xdg-runtime-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        });
    }
}
