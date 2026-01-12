#![cfg(not(target_arch = "wasm32"))]

mod harness;

use std::time::Duration;

use anyhow::Result;

#[tokio::test]
async fn freedos_boot_smoke() -> Result<()> {
    let freedos_img = harness::repo_root().join("test-images/freedos/fd14-boot-aero.img");
    harness::ensure_ci_prereq(
        &freedos_img,
        "Run `bash ./scripts/prepare-freedos.sh` to download + patch FreeDOS test media.",
    )?;
    if !freedos_img.exists() {
        return Ok(());
    }

    let Some(vm) = harness::QemuVm::spawn(harness::QemuConfig {
        memory_mib: 64,
        floppy: Some(freedos_img.clone()),
        ..Default::default()
    })
    .await?
    else {
        return Ok(());
    };

    vm.wait_for_serial_contains("AERO_FREEDOS_OK", Duration::from_secs(45))
        .await?;

    vm.shutdown().await?;
    Ok(())
}
