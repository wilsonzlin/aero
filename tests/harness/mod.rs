#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, io::Write};

use anyhow::{anyhow, Context, Result};
use image::RgbaImage;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};

#[derive(Clone, Default)]
pub struct ImageMatchConfig {
    /// Absolute per-channel tolerance (`|actual - expected| <= tolerance`).
    pub tolerance: u8,
    /// Fraction of pixels allowed to exceed `tolerance` (0.0-1.0).
    pub max_mismatch_ratio: f32,
    /// Optional crop applied to both images before comparison.
    pub crop: Option<ImageCrop>,
    /// Controls whether `wait_for_screenshot_match` writes comparison artifacts on mismatch.
    pub artifacts: ImageMatchArtifacts,
}

#[derive(Clone, Debug)]
pub struct ImageMatchArtifacts {
    /// Whether to emit artifacts on a mismatch/timeout.
    pub enabled: bool,
    /// Optional label used in artifact filenames (e.g. a test name).
    ///
    /// If unset, `wait_for_screenshot_match` falls back to the golden image filename stem.
    pub name: Option<String>,
}

impl Default for ImageMatchArtifacts {
    fn default() -> Self {
        Self {
            enabled: true,
            name: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ImageCrop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct ImageDiff {
    pub mismatched_pixels: u64,
    pub total_pixels: u64,
    pub max_channel_diff: u8,
}

impl ImageDiff {
    pub fn mismatch_ratio(&self) -> f32 {
        if self.total_pixels == 0 {
            return 0.0;
        }
        (self.mismatched_pixels as f32) / (self.total_pixels as f32)
    }
}

pub fn compare_images(
    actual: &RgbaImage,
    expected: &RgbaImage,
    cfg: &ImageMatchConfig,
) -> Result<ImageDiff> {
    let (actual, expected) = normalize_images_for_comparison(actual, expected, cfg)?;

    let mut mismatched_pixels: u64 = 0;
    let mut max_channel_diff: u8 = 0;

    for (a, e) in actual.pixels().zip(expected.pixels()) {
        let mut pixel_diff_max: u8 = 0;
        for ch in 0..4 {
            let da = a.0[ch].abs_diff(e.0[ch]);
            pixel_diff_max = pixel_diff_max.max(da);
        }
        max_channel_diff = max_channel_diff.max(pixel_diff_max);
        if pixel_diff_max > cfg.tolerance {
            mismatched_pixels += 1;
        }
    }

    Ok(ImageDiff {
        mismatched_pixels,
        total_pixels: (actual.width() as u64) * (actual.height() as u64),
        max_channel_diff,
    })
}

/// Render a diff image where pixels matching within `cfg.tolerance` keep the expected pixel, and
/// mismatched pixels are highlighted in red.
pub fn render_image_diff(actual: &RgbaImage, expected: &RgbaImage, cfg: &ImageMatchConfig) -> RgbaImage {
    // We intentionally keep this helper infallible so callers can always get *some* output
    // even if normalization fails (dimension mismatch/crop bounds/etc.). In that case, return
    // the expected image as-is.
    let Ok((actual, expected)) = normalize_images_for_comparison(actual, expected, cfg) else {
        return expected.clone();
    };
    render_image_diff_normalized(&actual, &expected, cfg.tolerance)
}

fn render_image_diff_normalized(actual: &RgbaImage, expected: &RgbaImage, tolerance: u8) -> RgbaImage {
    let mut out = expected.clone();
    for (o, (a, e)) in out
        .pixels_mut()
        .zip(actual.pixels().zip(expected.pixels()))
    {
        let mut pixel_diff_max: u8 = 0;
        for ch in 0..4 {
            pixel_diff_max = pixel_diff_max.max(a.0[ch].abs_diff(e.0[ch]));
        }
        if pixel_diff_max > tolerance {
            *o = image::Rgba([255, 0, 0, 255]);
        }
    }
    out
}

fn normalize_images_for_comparison(
    actual: &RgbaImage,
    expected: &RgbaImage,
    cfg: &ImageMatchConfig,
) -> Result<(RgbaImage, RgbaImage)> {
    let mut actual = actual.clone();
    let mut expected = expected.clone();

    if actual.dimensions() != expected.dimensions() {
        if let Some(rescaled_actual) =
            rescale_integer_multiple(&actual, expected.width(), expected.height())
        {
            actual = rescaled_actual;
        } else if let Some(rescaled_expected) =
            rescale_integer_multiple(&expected, actual.width(), actual.height())
        {
            expected = rescaled_expected;
        }
    }

    if actual.dimensions() != expected.dimensions() {
        return Err(anyhow!(
            "image dimensions differ: actual={}x{}, expected={}x{}",
            actual.width(),
            actual.height(),
            expected.width(),
            expected.height(),
        ));
    }

    if let Some(crop) = cfg.crop {
        actual = crop_image(&actual, crop)?;
        expected = crop_image(&expected, crop)?;
    }

    Ok((actual, expected))
}

fn rescale_integer_multiple(image: &RgbaImage, target_w: u32, target_h: u32) -> Option<RgbaImage> {
    if image.width() == target_w && image.height() == target_h {
        return Some(image.clone());
    }

    // Downscale if the source is an integer multiple of the target.
    if target_w != 0
        && target_h != 0
        && image.width().is_multiple_of(target_w)
        && image.height().is_multiple_of(target_h)
    {
        let sx = image.width() / target_w;
        let sy = image.height() / target_h;
        if sx == sy && sx >= 2 {
            return Some(resample_downscale_nearest(image, target_w, target_h, sx));
        }
    }

    // Upscale if the target is an integer multiple of the source.
    if image.width() != 0
        && image.height() != 0
        && target_w.is_multiple_of(image.width())
        && target_h.is_multiple_of(image.height())
    {
        let sx = target_w / image.width();
        let sy = target_h / image.height();
        if sx == sy && sx >= 2 {
            return Some(resample_upscale_nearest(image, target_w, target_h, sx));
        }
    }

    None
}

fn resample_downscale_nearest(
    image: &RgbaImage,
    target_w: u32,
    target_h: u32,
    scale: u32,
) -> RgbaImage {
    let mut out = RgbaImage::new(target_w, target_h);
    for y in 0..target_h {
        for x in 0..target_w {
            let src_x = x * scale;
            let src_y = y * scale;
            out.put_pixel(x, y, *image.get_pixel(src_x, src_y));
        }
    }
    out
}

fn resample_upscale_nearest(
    image: &RgbaImage,
    target_w: u32,
    target_h: u32,
    scale: u32,
) -> RgbaImage {
    let mut out = RgbaImage::new(target_w, target_h);
    for y in 0..target_h {
        for x in 0..target_w {
            let src_x = x / scale;
            let src_y = y / scale;
            out.put_pixel(x, y, *image.get_pixel(src_x, src_y));
        }
    }
    out
}

fn crop_image(image: &RgbaImage, crop: ImageCrop) -> Result<RgbaImage> {
    if crop.x.saturating_add(crop.width) > image.width()
        || crop.y.saturating_add(crop.height) > image.height()
    {
        return Err(anyhow!(
            "crop {crop:?} out of bounds for image {}x{}",
            image.width(),
            image.height()
        ));
    }
    Ok(image::imageops::crop_imm(image, crop.x, crop.y, crop.width, crop.height).to_image())
}

pub fn artifact_dir() -> PathBuf {
    let dir = if let Some(dir) = std::env::var_os("AERO_ARTIFACT_DIR") {
        PathBuf::from(dir)
    } else {
        // Keep artifacts colocated with the Cargo `target/` directory so they are easy to find
        // and can be cleaned up with `rm -rf target/` (or by CI caching rules).
        //
        // If `CARGO_TARGET_DIR` is set, honor it to avoid writing into a different target dir than
        // the one used by the build/test invocation.
        let root = repo_root();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        let target_dir = if target_dir.is_absolute() {
            target_dir
        } else {
            root.join(target_dir)
        };
        target_dir.join("aero-test-artifacts")
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "warning: failed to create artifact dir {}: {err}",
            dir.display()
        );
    }
    dir
}

pub fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        let looks_like_repo_root = dir.join("Cargo.toml").is_file()
            && dir.join("crates").is_dir()
            && dir.join("tests").is_dir();
        if looks_like_repo_root {
            return dir;
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent.to_path_buf();
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sanitize_artifact_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "screenshot".to_string()
    } else {
        // Avoid pathological filenames in CI logs.
        trimmed.chars().take(80).collect()
    }
}

fn screenshot_artifact_label(golden: &Path, cfg: &ImageMatchConfig) -> String {
    if let Some(name) = cfg.artifacts.name.as_deref() {
        return sanitize_artifact_component(name);
    }

    // Not a standard env var set by `cargo test`, but some CI runners / custom harnesses set it.
    // Prefer it over the golden image stem when present so concurrent failures don't collide.
    for key in ["RUST_TEST_NAME", "NEXTEST_TEST_NAME", "NEXTEST_TEST_FULL_NAME"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim();
            if !v.is_empty() {
                return sanitize_artifact_component(v);
            }
        }
    }

    golden
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_artifact_component)
        .unwrap_or_else(|| "screenshot".to_string())
}

static SCREENSHOT_MISMATCH_ARTIFACT_SEQ: AtomicU64 = AtomicU64::new(1);

struct ScreenshotMismatchArtifacts {
    dir: PathBuf,
    actual_path: PathBuf,
    expected_path: PathBuf,
    diff_path: PathBuf,
    meta_path: PathBuf,
    warnings: Vec<String>,
}

fn write_screenshot_mismatch_artifacts(
    golden: &Path,
    cfg: &ImageMatchConfig,
    actual: &RgbaImage,
    expected: &RgbaImage,
    diff: &ImageDiff,
    qemu_cmdline: &[String],
    nonce: u64,
) -> ScreenshotMismatchArtifacts {
    let label = screenshot_artifact_label(golden, cfg);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let ts_ms = now.as_millis() as u64;
    let pid = std::process::id();
    let seq = SCREENSHOT_MISMATCH_ARTIFACT_SEQ.fetch_add(1, Ordering::Relaxed);

    let dir = artifact_dir().join(format!(
        "screenshot-mismatch-{label}-{ts_ms}-pid{pid}-seq{seq}-n{nonce}"
    ));

    let actual_path = dir.join("actual.png");
    let expected_path = dir.join("expected.png");
    let diff_path = dir.join("diff.png");
    let meta_path = dir.join("mismatch.json");

    let mut warnings: Vec<String> = Vec::new();

    if let Err(err) = std::fs::create_dir_all(&dir) {
        warnings.push(format!(
            "failed to create artifact dir {}: {err}",
            dir.display()
        ));
        return ScreenshotMismatchArtifacts {
            dir,
            actual_path,
            expected_path,
            diff_path,
            meta_path,
            warnings,
        };
    }

    let (actual_norm, expected_norm, normalized_ok) =
        match normalize_images_for_comparison(actual, expected, cfg) {
            Ok((a, e)) => (a, e, true),
            Err(err) => {
                warnings.push(format!(
                    "failed to normalize images for artifact output: {err}"
                ));
                (actual.clone(), expected.clone(), false)
            }
        };

    if let Err(err) = actual_norm.save(&actual_path) {
        warnings.push(format!(
            "failed to save actual screenshot artifact {}: {err}",
            actual_path.display()
        ));
    }
    if let Err(err) = expected_norm.save(&expected_path) {
        warnings.push(format!(
            "failed to save expected screenshot artifact {}: {err}",
            expected_path.display()
        ));
    }

    let diff_img = if normalized_ok {
        render_image_diff_normalized(&actual_norm, &expected_norm, cfg.tolerance)
    } else {
        render_image_diff(actual, expected, cfg)
    };
    if let Err(err) = diff_img.save(&diff_path) {
        warnings.push(format!(
            "failed to save diff artifact {}: {err}",
            diff_path.display()
        ));
    }

    let meta = json!({
        "type": "aero_screenshot_mismatch",
        "label": label,
        "golden_path": golden.display().to_string(),
        "timestamp_unix_secs": now.as_secs(),
        "timestamp_unix_nanos": now.subsec_nanos(),
        "timestamp_unix_ms": ts_ms,
        "pid": pid,
        "seq": seq,
        "nonce": nonce,
        "comparison": {
            "mismatch_ratio": diff.mismatch_ratio(),
            "mismatched_pixels": diff.mismatched_pixels,
            "total_pixels": diff.total_pixels,
            "max_channel_diff": diff.max_channel_diff,
        },
        "config": {
            "tolerance": cfg.tolerance,
            "max_mismatch_ratio": cfg.max_mismatch_ratio,
            "crop": cfg.crop.map(|c| json!({
                "x": c.x,
                "y": c.y,
                "width": c.width,
                "height": c.height,
            })),
            "artifacts": {
                "enabled": cfg.artifacts.enabled,
                "name": cfg.artifacts.name.as_deref(),
            },
        },
        "qemu_cmdline": qemu_cmdline,
        "qemu_cmdline_string": qemu_cmdline.join(" "),
        "artifacts": {
            "dir": dir.display().to_string(),
            "actual_png": actual_path.display().to_string(),
            "expected_png": expected_path.display().to_string(),
            "diff_png": diff_path.display().to_string(),
            "meta_json": meta_path.display().to_string(),
        },
    });

    match serde_json::to_vec_pretty(&meta)
        .ok()
        .and_then(|bytes| std::fs::write(&meta_path, bytes).err())
    {
        Some(err) => warnings.push(format!(
            "failed to write metadata {}: {err}",
            meta_path.display()
        )),
        None => {}
    }

    ScreenshotMismatchArtifacts {
        dir,
        actual_path,
        expected_path,
        diff_path,
        meta_path,
        warnings,
    }
}

pub fn ensure_ci_prereq(path: &Path, how_to_fix: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    if std::env::var_os("AERO_REQUIRE_TEST_IMAGES").is_some() {
        return Err(anyhow!(
            "missing required test asset at {}.\n{how_to_fix}",
            path.display()
        ));
    }

    eprintln!(
        "skipping: missing test asset at {}.\n{how_to_fix}",
        path.display()
    );
    Ok(())
}

#[derive(Clone, Default)]
pub struct QemuConfig {
    pub memory_mib: u32,
    pub floppy: Option<PathBuf>,
    pub hda: Option<PathBuf>,
    pub boot_order: Option<String>,
    pub extra_args: Vec<String>,
}

pub struct QemuVm {
    temp_dir: TempDir,
    child: Child,
    qmp: QmpClient,
    serial_path: PathBuf,
    stderr: LogCapture,
    qemu_cmdline: Vec<String>,
}

impl QemuVm {
    pub fn qemu_binary() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("AERO_QEMU") {
            return Some(PathBuf::from(path));
        }

        // Keep it simple: QEMU i386 is the lowest-common-denominator for our early boot tests.
        // If it isn't present, fall back to x86_64 (more commonly installed on dev machines).
        //
        // CI installs `qemu-system-i386` explicitly; this fallback mainly improves local
        // developer ergonomics.
        which::which("qemu-system-i386").or_else(|| which::which("qemu-system-x86_64"))
    }

    pub async fn spawn(cfg: QemuConfig) -> Result<Option<Self>> {
        let qemu = match Self::qemu_binary() {
            Some(path) => path,
            None => {
                eprintln!(
                    "skipping: qemu-system-i386/qemu-system-x86_64 not found (install QEMU or set AERO_QEMU=...)"
                );
                return Ok(None);
            }
        };

        let temp_dir = tempfile::Builder::new()
            .prefix("aero-qemu-")
            .tempdir()
            .context("create temp dir for QEMU")?;
        let qmp_path = temp_dir.path().join("qmp.sock");
        let serial_path = temp_dir.path().join("serial.log");

        let mut args: Vec<String> = Vec::new();
        args.extend_from_slice(&[
            // Always use TCG in CI/tests (GitHub-hosted runners typically lack KVM, and we
            // prefer deterministic behavior over host-accelerated execution).
            "-accel".to_string(),
            "tcg".to_string(),
            "-display".to_string(),
            "none".to_string(),
            "-serial".to_string(),
            format!("file:{}", serial_path.display()),
            "-monitor".to_string(),
            "none".to_string(),
            "-qmp".to_string(),
            format!("unix:{},server,nowait", qmp_path.display()),
            "-no-reboot".to_string(),
            "-net".to_string(),
            "none".to_string(),
            // Ensure images are never modified by tests.
            "-snapshot".to_string(),
        ]);

        if cfg.memory_mib != 0 {
            args.push("-m".to_string());
            args.push(cfg.memory_mib.to_string());
        }

        if let Some(floppy) = &cfg.floppy {
            args.push("-drive".to_string());
            args.push(format!("file={},if=floppy,format=raw", floppy.display()));
        }

        if let Some(hda) = &cfg.hda {
            args.push("-drive".to_string());
            args.push(format!("file={},if=ide,media=disk", hda.display()));
        }

        if let Some(order) = &cfg.boot_order {
            args.push("-boot".to_string());
            args.push(format!("order={order}"));
        } else if cfg.floppy.is_some() {
            args.push("-boot".to_string());
            args.push("order=a".to_string());
        } else if cfg.hda.is_some() {
            args.push("-boot".to_string());
            args.push("order=c".to_string());
        }

        for arg in &cfg.extra_args {
            args.push(arg.clone());
        }

        let mut qemu_cmdline: Vec<String> = Vec::with_capacity(1 + args.len());
        qemu_cmdline.push(qemu.display().to_string());
        qemu_cmdline.extend(args.iter().cloned());

        let mut cmd = Command::new(&qemu);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(&args);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {}", qemu.display()))?;

        let stderr = LogCapture::default();

        if let Some(out) = child.stdout.take() {
            let stderr_clone = stderr.clone();
            tokio::spawn(async move {
                // QEMU's stdout isn't expected to contain guest serial output in our headless
                // configuration (we direct COM1 to a file for CI reliability), but keeping it
                // around can help debug failures.
                stderr_clone.consume_reader(out).await;
            });
        }

        if let Some(err) = child.stderr.take() {
            let stderr_clone = stderr.clone();
            tokio::spawn(async move {
                stderr_clone.consume_reader(err).await;
            });
        }

        let mut qmp = QmpClient::connect(&qmp_path).await?;
        qmp.capabilities().await?;

        Ok(Some(Self {
            temp_dir,
            child,
            qmp,
            serial_path,
            stderr,
            qemu_cmdline,
        }))
    }

    pub async fn wait_for_serial_contains(&self, needle: &str, timeout: Duration) -> Result<()> {
        wait_for_file_contains(&self.serial_path, needle.as_bytes(), timeout)
            .await
            .map_err(|err| anyhow!("{err}\n\nstderr:\n{}", self.stderr.snapshot_lossy()))
    }

    pub async fn screenshot_rgba(&mut self) -> Result<RgbaImage> {
        let ppm_path = self
            .temp_dir
            .path()
            .join(format!("screenshot-{}.ppm", self.qmp.next_nonce()));
        self.qmp
            .execute(
                "screendump",
                Some(json!({ "filename": ppm_path.display().to_string() })),
            )
            .await
            .context("QMP screendump")?;

        load_image_rgba(&ppm_path)
            .with_context(|| format!("load PPM screendump {}", ppm_path.display()))
    }

    pub async fn wait_for_screenshot_match(
        &mut self,
        golden: &Path,
        timeout: Duration,
        cfg: &ImageMatchConfig,
    ) -> Result<()> {
        let expected = load_image_rgba(golden)
            .with_context(|| format!("load golden image {}", golden.display()))?;
        let deadline = Instant::now() + timeout;

        loop {
            let actual = self.screenshot_rgba().await?;
            let diff = compare_images(&actual, &expected, cfg)?;
            if diff.mismatch_ratio() <= cfg.max_mismatch_ratio {
                return Ok(());
            }

            if Instant::now() >= deadline {
                let mut msg = format!(
                    "screenshot did not match golden {} within {timeout:?}:\n  mismatched_pixels={} / {} ({:.4}), max_channel_diff={} (tolerance={}), allowed_mismatch_ratio={}",
                    golden.display(),
                    diff.mismatched_pixels,
                    diff.total_pixels,
                    diff.mismatch_ratio(),
                    diff.max_channel_diff,
                    cfg.tolerance,
                    cfg.max_mismatch_ratio,
                );

                if cfg.artifacts.enabled {
                    let nonce = self.qmp.next_nonce();
                    let artifacts = write_screenshot_mismatch_artifacts(
                        golden,
                        cfg,
                        &actual,
                        &expected,
                        &diff,
                        &self.qemu_cmdline,
                        nonce,
                    );
                    msg.push_str(&format!(
                        "\nartifacts:\n  dir: {}\n  actual:   {}\n  expected: {}\n  diff:     {}\n  meta:     {}",
                        artifacts.dir.display(),
                        artifacts.actual_path.display(),
                        artifacts.expected_path.display(),
                        artifacts.diff_path.display(),
                        artifacts.meta_path.display(),
                    ));
                    if !artifacts.warnings.is_empty() {
                        for warn in artifacts.warnings {
                            msg.push_str(&format!("\nwarning: {warn}"));
                        }
                    }
                } else {
                    msg.push_str("\nartifacts disabled (cfg.artifacts.enabled=false)");
                }
                return Err(anyhow!(msg));
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn wait_for_stable_screenshot(
        &mut self,
        timeout: Duration,
        poll_interval: Duration,
        stable_samples: usize,
        cfg: &ImageMatchConfig,
    ) -> Result<RgbaImage> {
        let deadline = Instant::now() + timeout;
        let mut last: Option<RgbaImage> = None;
        let mut stable: usize = 0;

        loop {
            let shot = self.screenshot_rgba().await?;
            if let Some(prev) = &last {
                let diff = compare_images(&shot, prev, cfg)?;
                if diff.mismatch_ratio() <= cfg.max_mismatch_ratio {
                    stable += 1;
                } else {
                    stable = 0;
                }
            }
            last = Some(shot.clone());

            if stable >= stable_samples {
                return Ok(shot);
            }

            if Instant::now() >= deadline {
                return Err(anyhow!("frame did not stabilize within {timeout:?}"));
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    pub async fn shutdown(mut self) -> Result<()> {
        // Prefer a graceful quit so QEMU flushes the screendump.
        let _ = self.qmp.execute("quit", None).await;
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .context("waiting for QEMU to exit")??;
        if !status.success() {
            return Err(anyhow!("QEMU exited with status: {status}"));
        }
        Ok(())
    }
}

impl Drop for QemuVm {
    fn drop(&mut self) {
        // Best-effort cleanup for panics/timeouts.
        let _ = self.child.start_kill();
    }
}

#[derive(Clone, Default)]
struct LogCapture {
    buf: std::sync::Arc<Mutex<Vec<u8>>>,
    notify: std::sync::Arc<Notify>,
}

impl LogCapture {
    async fn consume_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
        &self,
        mut reader: R,
    ) {
        let mut chunk = [0u8; 4096];
        loop {
            let n = match reader.read(&mut chunk).await {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => return,
            };
            {
                let mut buf = self.buf.lock().await;
                buf.extend_from_slice(&chunk[..n]);
            }
            self.notify.notify_waiters();
        }
    }

    async fn wait_for_substring(&self, needle: &[u8], timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;

        loop {
            {
                let buf = self.buf.lock().await;
                if find_subslice(&buf, needle) {
                    return Ok(());
                }
            }

            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out after {timeout:?} waiting for {:?} in log\nlog:\n{}",
                    String::from_utf8_lossy(needle),
                    self.snapshot_lossy()
                ));
            }

            let notified = self.notify.notified();
            tokio::select! {
                _ = notified => {},
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {},
            }
        }
    }

    fn snapshot_lossy(&self) -> String {
        let guard = self.buf.try_lock();
        match guard {
            Ok(buf) => String::from_utf8_lossy(&buf).to_string(),
            Err(_) => "<log busy>".to_string(),
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|win| win == needle)
}

async fn wait_for_file_contains(path: &Path, needle: &[u8], timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match tokio::fs::read(path).await {
            Ok(contents) => {
                if find_subslice(&contents, needle) {
                    return Ok(());
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // QEMU hasn't created the file yet.
            }
            Err(err) => {
                return Err(err).with_context(|| format!("read serial log {}", path.display()))
            }
        }

        if Instant::now() >= deadline {
            let log = tokio::fs::read(path)
                .await
                .ok()
                .map(|v| String::from_utf8_lossy(&v).to_string())
                .unwrap_or_else(|| "<unavailable>".to_string());

            return Err(anyhow!(
                "timed out after {timeout:?} waiting for {:?} in serial log {}\nserial:\n{log}",
                String::from_utf8_lossy(needle),
                path.display()
            ));
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn load_image_rgba(path: &Path) -> Result<RgbaImage> {
    Ok(image::ImageReader::open(path)?.decode()?.to_rgba8())
}

struct QmpClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: u64,
    next_nonce: u64,
}

impl QmpClient {
    async fn connect(path: &Path) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match UnixStream::connect(path).await {
                Ok(stream) => {
                    let (read_half, write_half) = stream.into_split();
                    let mut client = Self {
                        reader: BufReader::new(read_half),
                        writer: write_half,
                        next_id: 1,
                        next_nonce: 1,
                    };

                    // QMP sends a greeting as the first message; read and ignore it.
                    let _ = client.read_message().await?;
                    return Ok(client);
                }
                Err(err) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(err) => return Err(err).context("connect to QMP socket"),
            }
        }
    }

    fn next_nonce(&mut self) -> u64 {
        let n = self.next_nonce;
        self.next_nonce += 1;
        n
    }

    async fn capabilities(&mut self) -> Result<()> {
        let _ = self.execute("qmp_capabilities", None).await?;
        Ok(())
    }

    async fn execute(&mut self, cmd: &str, args: Option<Value>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let mut req = json!({
            "execute": cmd,
            "id": id,
        });
        if let Some(args) = args {
            req["arguments"] = args;
        }

        let payload = serde_json::to_vec(&req)?;
        self.writer.write_all(&payload).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        loop {
            let msg = self.read_message().await?;
            if msg.get("id").and_then(|v| v.as_u64()) != Some(id) {
                // Async event or response to a different command.
                continue;
            }

            if let Some(err) = msg.get("error") {
                return Err(anyhow!("QMP error executing {cmd}: {err}"));
            }

            return Ok(msg.get("return").cloned().unwrap_or(Value::Null));
        }
    }

    async fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(anyhow!("unexpected EOF from QMP"));
        }
        serde_json::from_str(&line).context("parse QMP JSON")
    }
}

pub fn write_floppy_image(path: &Path, bootsector: &[u8]) -> Result<()> {
    if bootsector.len() != 512 {
        return Err(anyhow!(
            "bootsector must be exactly 512 bytes, got {}",
            bootsector.len()
        ));
    }
    let mut file = fs::File::create(path)?;
    file.write_all(bootsector)?;
    file.set_len(1_474_560)?;
    Ok(())
}

// Minimal internal dependency so we don't have to add an external crate just to locate QEMU.
mod which {
    use std::path::{Path, PathBuf};

    pub fn which(name: &str) -> Option<PathBuf> {
        if name.contains(std::path::MAIN_SEPARATOR) {
            let path = PathBuf::from(name);
            return if is_executable(&path) {
                Some(path)
            } else {
                None
            };
        }

        let path_env = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_env) {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    fn is_executable(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            (path
                .metadata()
                .ok()
                .map(|m| m.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}
