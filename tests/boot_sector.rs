#![cfg(not(target_arch = "wasm32"))]

mod harness;

use std::time::Duration;

use anyhow::{Context, Result};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_sector_minimal_image() -> Result<()> {
    let bootsector = include_bytes!("fixtures/bootsector.bin");

    let temp = tempfile::tempdir().context("tempdir")?;
    let floppy_path = temp.path().join("bootsector.img");
    harness::write_floppy_image(&floppy_path, bootsector)?;

    let Some(mut vm) = harness::QemuVm::spawn(harness::QemuConfig {
        memory_mib: 32,
        floppy: Some(floppy_path),
        ..Default::default()
    })
    .await?
    else {
        return Ok(());
    };

    vm.wait_for_serial_contains("AERO_BOOTSECTOR_OK", Duration::from_secs(5))
        .await?;

    let golden = harness::repo_root().join("tests/golden/bootsector_mode13.png");
    let cfg = harness::ImageMatchConfig {
        tolerance: 0,
        max_mismatch_ratio: 0.0,
        crop: None,
    };
    vm.wait_for_screenshot_match(&golden, Duration::from_secs(5), &cfg)
        .await?;

    vm.shutdown().await?;
    Ok(())
}
