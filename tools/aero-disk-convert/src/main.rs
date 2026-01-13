use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use aero_storage::{AeroSparseConfig, AeroSparseDisk, DiskImage, FileBackend, VirtualDisk};
use anyhow::{anyhow, bail, Context};
use clap::Parser;
use serde::Serialize;

const DEFAULT_BLOCK_SIZE_BYTES: u32 = 1024 * 1024; // 1 MiB

// A safety guard: converting a disk requires scanning it block-by-block. Extremely large disks can
// cause accidental multi-hour conversions or huge output allocation tables.
const DEFAULT_ABSURD_DISK_SIZE_BYTES: u64 = 16 * 1024 * 1024 * 1024 * 1024; // 16 TiB
const DEFAULT_ABSURD_TABLE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

#[derive(Parser, Debug)]
#[command(
    name = "aero-disk-convert",
    about = "Convert raw/qcow2/vhd disk images into Aero sparse (AEROSPAR), optionally creating a COW overlay."
)]
struct Args {
    /// Input disk image path (raw/qcow2/vhd; auto-detected)
    input: PathBuf,

    /// Output Aero sparse disk path (.aerospar)
    output: PathBuf,

    /// Output virtual disk size in bytes (defaults to input capacity)
    #[arg(long, value_name = "BYTES")]
    disk_size_bytes: Option<u64>,

    /// Aero sparse allocation unit size in bytes (power of two; multiple of 512)
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_BLOCK_SIZE_BYTES)]
    block_size_bytes: u32,

    /// Create a writable Aero sparse overlay (copy-on-write) next to the base image
    #[arg(long, action = clap::ArgAction::SetTrue)]
    overlay: bool,

    /// Override overlay output path (defaults to "<output stem>.overlay.aerospar")
    #[arg(long, value_name = "PATH")]
    overlay_path: Option<PathBuf>,

    /// Write metadata JSON to this path (defaults to stdout when --overlay is used)
    #[arg(long, value_name = "PATH")]
    metadata_out: Option<PathBuf>,

    /// Suppress progress output
    #[arg(long, action = clap::ArgAction::SetTrue)]
    quiet: bool,

    /// Allow overwriting outputs and bypass safety checks
    #[arg(long, action = clap::ArgAction::SetTrue)]
    force: bool,
}

/// Output metadata intended to be copy-pasteable into the web disk manager.
///
/// The `disk` entry follows the shape of `LocalDiskImageMetadata` in
/// `web/src/storage/metadata.ts`. The optional `overlay` entry matches the overlay metadata shape
/// used in runtime snapshots (`RuntimeDiskSnapshotEntry`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDiskConvertMetadata {
    version: u32,
    disk: WebLocalDiskMetadata,
    overlay: WebCowOverlayMetadata,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebLocalDiskMetadata {
    source: &'static str,
    id: String,
    name: String,
    backend: &'static str,
    kind: &'static str,
    format: &'static str,
    file_name: String,
    size_bytes: u64,
    created_at_ms: u64,
    source_file_name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebCowOverlayMetadata {
    file_name: String,
    disk_size_bytes: u64,
    block_size_bytes: u32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    run(args)
}

fn run(args: Args) -> anyhow::Result<()> {
    validate_block_size(args.block_size_bytes)?;

    let input_backend = FileBackend::open_read_only(&args.input)
        .with_context(|| format!("open input {}", args.input.display()))?;
    let mut input_disk =
        DiskImage::open_auto(input_backend).context("open input disk (auto-detect)")?;

    let input_capacity = input_disk.capacity_bytes();
    validate_disk_size("input", input_capacity)?;

    let disk_size = args.disk_size_bytes.unwrap_or(input_capacity);
    validate_disk_size("output", disk_size)?;
    if disk_size < input_capacity && !args.force {
        bail!(
            "refusing to shrink disk: input capacity is {input_capacity} bytes, but --disk-size-bytes is {disk_size} bytes (use --force to override)"
        );
    }

    enforce_absurd_limits(disk_size, args.block_size_bytes, args.force)?;

    let output_backend =
        open_output_file(&args.output, args.force).with_context(|| format!("create {}", args.output.display()))?;
    let mut output_disk = AeroSparseDisk::create(
        output_backend,
        AeroSparseConfig {
            disk_size_bytes: disk_size,
            block_size_bytes: args.block_size_bytes,
        },
    )
    .context("create output aerospar")?;

    if !args.quiet {
        eprintln!(
            "input:  {} ({} bytes, {:?})",
            args.input.display(),
            input_capacity,
            input_disk.format()
        );
        eprintln!(
            "output: {} ({} bytes, block={} bytes)",
            args.output.display(),
            disk_size,
            args.block_size_bytes
        );
    }

    copy_nonzero_blocks(
        &mut input_disk,
        &mut output_disk,
        input_capacity,
        disk_size,
        args.block_size_bytes,
        args.quiet,
    )?;

    output_disk.flush().context("flush output")?;

    if args.overlay {
        let disk_id = default_disk_id(&args.output);
        let overlay_path = args
            .overlay_path
            .clone()
            .unwrap_or_else(|| default_overlay_path(&args.output, &disk_id));
        let overlay_backend = open_output_file(&overlay_path, args.force)
            .with_context(|| format!("create {}", overlay_path.display()))?;
        let mut overlay_disk = AeroSparseDisk::create(
            overlay_backend,
            AeroSparseConfig {
                disk_size_bytes: disk_size,
                block_size_bytes: args.block_size_bytes,
            },
        )
        .context("create overlay aerospar")?;
        overlay_disk.flush().context("flush overlay")?;

        let created_at_ms = now_unix_ms();
        let disk_file_name = basename_string(&args.output);
        let overlay_file_name = basename_string(&overlay_path);
        let disk_name = args
            .input
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| disk_id.clone());

        let meta = WebDiskConvertMetadata {
            version: 1,
            disk: WebLocalDiskMetadata {
                source: "local",
                id: disk_id.clone(),
                name: disk_name,
                backend: "opfs",
                kind: "hdd",
                format: "aerospar",
                file_name: disk_file_name,
                size_bytes: disk_size,
                created_at_ms,
                source_file_name: args
                    .input
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string()),
            },
            overlay: WebCowOverlayMetadata {
                file_name: overlay_file_name,
                disk_size_bytes: disk_size,
                block_size_bytes: args.block_size_bytes,
            },
        };

        let json = serde_json::to_string_pretty(&meta).context("serialize metadata")?;
        if let Some(path) = &args.metadata_out {
            fs::write(path, json.as_bytes())
                .with_context(|| format!("write metadata {}", path.display()))?;
        } else {
            println!("{json}");
        }
    }

    Ok(())
}

fn now_unix_ms() -> u64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0));
    dur.as_millis().min(u128::from(u64::MAX)) as u64
}

fn basename_string(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn default_disk_id(output: &Path) -> String {
    let raw = output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("disk");
    let trimmed = raw.trim();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        "disk".to_string()
    } else {
        out
    }
}

fn validate_disk_size(label: &str, size_bytes: u64) -> anyhow::Result<()> {
    if size_bytes == 0 {
        bail!("{label} disk size must be non-zero");
    }
    if size_bytes % 512 != 0 {
        bail!("{label} disk size must be a multiple of 512 bytes (got {size_bytes})");
    }
    Ok(())
}

fn validate_block_size(block_size_bytes: u32) -> anyhow::Result<()> {
    if block_size_bytes == 0 {
        bail!("block size must be non-zero");
    }
    if (block_size_bytes as u64) % 512 != 0 {
        bail!("block size must be a multiple of 512 bytes (got {block_size_bytes})");
    }
    if !block_size_bytes.is_power_of_two() {
        bail!("block size must be a power of two (got {block_size_bytes})");
    }
    Ok(())
}

fn enforce_absurd_limits(disk_size_bytes: u64, block_size_bytes: u32, force: bool) -> anyhow::Result<()> {
    if force {
        return Ok(());
    }

    if disk_size_bytes > DEFAULT_ABSURD_DISK_SIZE_BYTES {
        bail!(
            "refusing to convert an extremely large disk ({} bytes > {} bytes); use --force to override",
            disk_size_bytes,
            DEFAULT_ABSURD_DISK_SIZE_BYTES
        );
    }

    let table_entries = div_ceil_u64(disk_size_bytes, block_size_bytes as u64);
    let table_bytes = table_entries
        .checked_mul(8)
        .ok_or_else(|| anyhow!("allocation table size overflow"))?;
    if table_bytes > DEFAULT_ABSURD_TABLE_BYTES {
        bail!(
            "refusing to create a very large allocation table ({} bytes > {} bytes); use --force to override (or increase --block-size-bytes)",
            table_bytes,
            DEFAULT_ABSURD_TABLE_BYTES
        );
    }

    Ok(())
}

fn div_ceil_u64(n: u64, d: u64) -> u64 {
    // d is expected > 0 (validated elsewhere)
    (n + d - 1) / d
}

fn copy_nonzero_blocks(
    input: &mut dyn VirtualDisk,
    output: &mut dyn VirtualDisk,
    input_capacity_bytes: u64,
    disk_size_bytes: u64,
    block_size_bytes: u32,
    quiet: bool,
) -> anyhow::Result<()> {
    let block_size_usize: usize = block_size_bytes
        .try_into()
        .map_err(|_| anyhow!("block size does not fit in usize"))?;

    let total_blocks = div_ceil_u64(disk_size_bytes, block_size_bytes as u64);
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(block_size_usize)
        .map_err(|_| anyhow!("unable to allocate {}-byte block buffer", block_size_bytes))?;
    buf.resize(block_size_usize, 0);

    let mut processed = 0u64;
    let mut last_report = Instant::now();

    for block_idx in 0..total_blocks {
        let offset = block_idx
            .checked_mul(block_size_bytes as u64)
            .ok_or_else(|| anyhow!("offset overflow"))?;
        let remaining = disk_size_bytes - offset;
        let len_u64 = (block_size_bytes as u64).min(remaining);
        let len: usize = len_u64
            .try_into()
            .map_err(|_| anyhow!("block length does not fit in usize"))?;

        let src_read_len_u64 = if offset >= input_capacity_bytes {
            0
        } else {
            len_u64.min(input_capacity_bytes - offset)
        };
        let src_read_len: usize = src_read_len_u64
            .try_into()
            .map_err(|_| anyhow!("read length does not fit in usize"))?;

        if src_read_len == 0 {
            // Beyond end of input (only possible when the output disk is larger than the input).
            processed = processed
                .checked_add(len_u64)
                .ok_or_else(|| anyhow!("processed overflow"))?;
            maybe_report_progress(&mut last_report, processed, disk_size_bytes, quiet)?;
            continue;
        }

        // If we are reading a partial block (either because we're at EOF of the disk or because
        // the output disk is larger than the input), ensure the remainder reads as zero.
        if src_read_len != len {
            buf[..len].fill(0);
        }

        input
            .read_at(offset, &mut buf[..src_read_len])
            .with_context(|| format!("read at offset={offset}"))?;

        if !is_all_zero(&buf[..src_read_len]) {
            output
                .write_at(offset, &buf[..src_read_len])
                .with_context(|| format!("write at offset={offset}"))?;
        }

        processed = processed
            .checked_add(len_u64)
            .ok_or_else(|| anyhow!("processed overflow"))?;
        maybe_report_progress(&mut last_report, processed, disk_size_bytes, quiet)?;
    }

    if !quiet {
        eprintln!();
    }

    Ok(())
}

fn maybe_report_progress(
    last_report: &mut Instant,
    processed: u64,
    total: u64,
    quiet: bool,
) -> io::Result<()> {
    if quiet {
        return Ok(());
    }
    let now = Instant::now();
    if processed == total || now.duration_since(*last_report) >= Duration::from_millis(250) {
        *last_report = now;
        let pct = if total == 0 {
            100u64
        } else {
            ((processed as u128).saturating_mul(100) / total as u128) as u64
        };
        eprint!("\rprogress: {pct:3}% ({processed}/{total} bytes)");
        io::stderr().flush()?;
    }
    Ok(())
}

fn is_all_zero(buf: &[u8]) -> bool {
    // SAFETY: We only reinterpret the bytes as `u64`. All bit patterns are valid `u64` values,
    // so this is safe.
    let (prefix, words, suffix) = unsafe { buf.align_to::<u64>() };
    prefix.iter().all(|&b| b == 0)
        && words.iter().all(|&w| w == 0)
        && suffix.iter().all(|&b| b == 0)
}

fn open_output_file(path: &Path, force: bool) -> anyhow::Result<FileBackend> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true);
    if force {
        opts.create(true).truncate(true);
    } else {
        opts.create_new(true);
    }
    let file = opts.open(path)?;
    Ok(FileBackend::from_file_with_path(file, path))
}

fn default_overlay_path(base: &Path, disk_id: &str) -> PathBuf {
    let parent = base.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{disk_id}.overlay.aerospar"))
}
