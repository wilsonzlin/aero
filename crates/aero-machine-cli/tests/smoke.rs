#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::process::Command;

use aero_storage::{AeroSparseConfig, AeroSparseDisk, FileBackend, VirtualDisk, SECTOR_SIZE};

fn resolve_cli_exe(repo_root: &PathBuf) -> PathBuf {
    // Avoid relying on `CARGO_BIN_EXE_*` (Cargo does not guarantee it is set for all test
    // invocation modes). Use the workspace `target/` dir path instead.
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("target"));
    let exe_name = format!("aero-machine{}", std::env::consts::EXE_SUFFIX);
    let debug_exe = target_dir.join("debug").join(&exe_name);
    let release_exe = target_dir.join("release").join(&exe_name);
    if debug_exe.exists() {
        debug_exe
    } else if release_exe.exists() {
        release_exe
    } else {
        panic!(
            "expected aero-machine binary at {} or {}",
            debug_exe.display(),
            release_exe.display()
        );
    }
}

fn assert_output_contains_boot_banner(output: &std::process::Output) {
    let expected = b"AERO!\r\n";
    assert!(
        output
            .stdout
            .windows(expected.len())
            .any(|w| w == expected),
        "stdout did not contain expected serial bytes.\nstdout:\n{:?}\nstderr:\n{}",
        output.stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_png_written(path: &std::path::Path) {
    let png = std::fs::read(path).expect("expected vga.png to be written");
    assert!(
        png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "vga.png did not look like a PNG (first bytes = {:?})",
        &png.get(..8)
    );
}

#[test]
fn boots_fixture_and_prints_serial() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let disk = repo_root.join("tests/fixtures/boot/boot_vga_serial_8s.img");
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let tmp_disk = tmp.path().join("disk.img");
    std::fs::copy(&disk, &tmp_disk).expect("failed to copy disk fixture");
    let tmp_png = tmp.path().join("vga.png");

    let exe = resolve_cli_exe(&repo_root);

    let output = Command::new(exe)
        .args([
            "--disk",
            tmp_disk.to_str().expect("disk path should be UTF-8"),
            "--disk-ro",
            "--ram",
            "64",
            "--max-insts",
            "100000",
            "--serial-out",
            "stdout",
            "--vga-png",
            tmp_png.to_str().expect("png path should be UTF-8"),
        ])
        .output()
        .expect("failed to run aero-machine CLI");

    assert!(
        output.status.success(),
        "aero-machine exited with {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert_output_contains_boot_banner(&output);
    assert_png_written(&tmp_png);
}

#[test]
fn boots_with_cow_overlay_and_creates_overlay_file() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let disk = repo_root.join("tests/fixtures/boot/boot_vga_serial_8s.img");
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let overlay = tmp.path().join("overlay.aerospar");
    let tmp_png = tmp.path().join("vga.png");

    let exe = resolve_cli_exe(&repo_root);

    let output = Command::new(exe)
        .args([
            "--disk",
            disk.to_str().expect("disk path should be UTF-8"),
            "--disk-overlay",
            overlay.to_str().expect("overlay path should be UTF-8"),
            "--ram",
            "64",
            "--max-insts",
            "100000",
            "--serial-out",
            "stdout",
            "--vga-png",
            tmp_png.to_str().expect("png path should be UTF-8"),
        ])
        .output()
        .expect("failed to run aero-machine CLI");

    assert!(
        output.status.success(),
        "aero-machine exited with {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert_output_contains_boot_banner(&output);
    assert_png_written(&tmp_png);

    let overlay_bytes = std::fs::read(&overlay).expect("expected overlay file to be written");
    assert!(
        overlay_bytes.starts_with(b"AEROSPAR"),
        "overlay did not start with AEROSPAR magic: {:?}",
        &overlay_bytes.get(..8)
    );
}

#[test]
fn boots_aerospar_disk_image() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let disk = repo_root.join("tests/fixtures/boot/boot_vga_serial_8s.img");
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let tmp_disk = tmp.path().join("disk.aerospar");
    let tmp_png = tmp.path().join("vga.png");

    let bytes = std::fs::read(&disk).expect("failed to read disk fixture");
    assert!(
        (bytes.len() as u64).is_multiple_of(SECTOR_SIZE as u64),
        "fixture length must be 512-byte aligned"
    );

    let backend = FileBackend::create(&tmp_disk, 0).expect("failed to create aerospar file");
    let mut aerospar = AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes: bytes.len() as u64,
            block_size_bytes: 4096,
        },
    )
    .expect("failed to create aerospar disk");
    aerospar.write_at(0, &bytes).expect("failed to write aerospar");
    aerospar.flush().expect("failed to flush aerospar");

    let exe = resolve_cli_exe(&repo_root);
    let output = Command::new(exe)
        .args([
            "--disk",
            tmp_disk.to_str().expect("disk path should be UTF-8"),
            "--disk-ro",
            "--ram",
            "64",
            "--max-insts",
            "100000",
            "--serial-out",
            "stdout",
            "--vga-png",
            tmp_png.to_str().expect("png path should be UTF-8"),
        ])
        .output()
        .expect("failed to run aero-machine CLI");

    assert!(
        output.status.success(),
        "aero-machine exited with {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert_output_contains_boot_banner(&output);
    assert_png_written(&tmp_png);
}

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn vhd_footer_checksum(raw: &[u8; SECTOR_SIZE]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in raw.iter().enumerate() {
        if (64..68).contains(&i) {
            continue;
        }
        sum = sum.wrapping_add(*b as u32);
    }
    !sum
}

fn make_vhd_footer(virtual_size: u64) -> [u8; SECTOR_SIZE] {
    let mut footer = [0u8; SECTOR_SIZE];
    footer[0..8].copy_from_slice(b"conectix");
    write_be_u32(&mut footer, 8, 2); // features
    write_be_u32(&mut footer, 12, 0x0001_0000); // file_format_version
    write_be_u64(&mut footer, 16, u64::MAX); // data_offset (fixed)
    write_be_u64(&mut footer, 40, virtual_size); // original_size
    write_be_u64(&mut footer, 48, virtual_size); // current_size
    write_be_u32(&mut footer, 60, 2); // disk_type=fixed
    let checksum = vhd_footer_checksum(&footer);
    write_be_u32(&mut footer, 64, checksum);
    footer
}

#[test]
fn boots_vhd_fixed_disk_image() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let disk = repo_root.join("tests/fixtures/boot/boot_vga_serial_8s.img");
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let tmp_disk = tmp.path().join("disk.vhd");
    let tmp_png = tmp.path().join("vga.png");

    let mut data = std::fs::read(&disk).expect("failed to read disk fixture");
    assert!(
        (data.len() as u64).is_multiple_of(SECTOR_SIZE as u64),
        "fixture length must be 512-byte aligned"
    );
    let virtual_size = data.len() as u64;
    let footer = make_vhd_footer(virtual_size);
    data.extend_from_slice(&footer);
    std::fs::write(&tmp_disk, &data).expect("failed to write vhd fixture");

    let exe = resolve_cli_exe(&repo_root);
    let output = Command::new(exe)
        .args([
            "--disk",
            tmp_disk.to_str().expect("disk path should be UTF-8"),
            "--disk-ro",
            "--ram",
            "64",
            "--max-insts",
            "100000",
            "--serial-out",
            "stdout",
            "--vga-png",
            tmp_png.to_str().expect("png path should be UTF-8"),
        ])
        .output()
        .expect("failed to run aero-machine CLI");

    assert!(
        output.status.success(),
        "aero-machine exited with {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert_output_contains_boot_banner(&output);
    assert_png_written(&tmp_png);
}
