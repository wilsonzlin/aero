#![cfg(not(target_arch = "wasm32"))]

mod harness;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a user-supplied Windows 7 disk image; see scripts/prepare-windows7.sh"]
async fn windows7_boot_placeholder() -> Result<()> {
    let Some(mut vm) = harness::QemuVm::spawn(harness::QemuConfig {
        memory_mib: 2048,
        hda: Some(resolve_windows_image()?),
        ..Default::default()
    })
    .await?
    else {
        return Ok(());
    };

    // The screenshot stability heuristic is a placeholder until we have higher-level
    // guest introspection. If you provide a golden image, we'll compare against it.
    let stable_cfg = harness::ImageMatchConfig {
        tolerance: 2,
        max_mismatch_ratio: 0.001,
        crop: None,
    };
    let shot = vm
        .wait_for_stable_screenshot(
            Duration::from_secs(120),
            Duration::from_secs(5),
            2,
            &stable_cfg,
        )
        .await?;

    let golden = resolve_windows_golden();
    if golden.exists() {
        let expected = image::ImageReader::open(&golden)?.decode()?.to_rgba8();
        let diff = harness::compare_images(&shot, &expected, &stable_cfg)?;
        if diff.mismatch_ratio() > stable_cfg.max_mismatch_ratio {
            return Err(anyhow!(
                "windows7 screenshot mismatch: mismatch_ratio={:.4}, max_channel_diff={}",
                diff.mismatch_ratio(),
                diff.max_channel_diff
            ));
        }
    } else {
        let dir = harness::artifact_dir();
        let path = dir.join("windows7_actual.png");
        shot.save(&path)?;
        return Err(anyhow!(
            "no golden image found at {}. Captured screenshot written to {}",
            golden.display(),
            path.display()
        ));
    }

    vm.shutdown().await?;
    Ok(())
}

fn resolve_windows_image() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AERO_WINDOWS7_IMAGE") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
        return Err(anyhow!(
            "AERO_WINDOWS7_IMAGE points to a missing path: {}",
            path.display()
        ));
    }

    let path = harness::repo_root().join("test-images/local/windows7.img");
    if !path.exists() {
        return Err(anyhow!(
            "missing Windows image at {} (set AERO_WINDOWS7_IMAGE to override)",
            path.display()
        ));
    }
    Ok(path)
}

fn resolve_windows_golden() -> PathBuf {
    std::env::var_os("AERO_WINDOWS7_GOLDEN")
        .map(PathBuf::from)
        .unwrap_or_else(|| harness::repo_root().join("test-images/local/windows7_login.png"))
}
